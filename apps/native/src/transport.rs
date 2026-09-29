//! Transport (DESIGN §5.3–5.5, §7.2–7.4): the sync WebSocket with heartbeats, backoff and an HTTP
//! long-poll fallback, plus the blob channel (chunked uploads, ranged downloads) on its own HTTP
//! requests so it never delays note sync. Mirrors the web worker's transport.

use crate::{now_ms, Native, VERSION};
use futures_util::{SinkExt, StreamExt};
use jess_core::blobs::{Bitmap, BlobResult, BlobTask};
use jess_core::client::reconnect_delay_ms;
use jess_core::proto::{self, ClientMsg, HttpSyncRequest, HttpSyncResponse, ServerMsg};
use jess_core::Hash;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

pub(crate) fn spawn(n: Native) {
    n.inner.rt.spawn(sync_loop(n.clone()));
    n.inner.rt.spawn(pump_loop(n.clone()));
}

fn ws_url(server: &str) -> String {
    let s = server.trim_end_matches('/');
    let s = if let Some(r) = s.strip_prefix("https://") {
        format!("wss://{r}")
    } else if let Some(r) = s.strip_prefix("http://") {
        format!("ws://{r}")
    } else {
        s.to_string()
    };
    format!("{s}/api/sync")
}

async fn sync_loop(n: Native) {
    let mut attempt = 0u32;
    let mut ws_failures = 0u32;
    loop {
        let c = n.inner.conf.lock().expect("lock").clone();
        let (Some(server), Some(token)) = (c.server.clone(), c.token.clone()) else {
            n.inner.wake_transport.notified().await;
            continue;
        };
        if c.fatal.is_some() {
            n.inner.wake_transport.notified().await;
            continue;
        }
        let http_mode = n.inner.st.lock().expect("lock").http_mode;
        if http_mode {
            http_loop(&n, &token).await;
        } else {
            let conn = tokio::time::timeout(
                Duration::from_secs(10),
                tokio_tungstenite::connect_async(ws_url(&server)),
            )
            .await;
            match conn {
                Ok(Ok((ws, _))) => {
                    ws_failures = 0;
                    if run_socket(&n, ws, &token).await {
                        attempt = 0;
                    }
                }
                _ => {
                    ws_failures += 1;
                    // Proxies that break WebSocket upgrades (§5.3): fall back to long-polling.
                    if ws_failures >= 3 {
                        n.inner.st.lock().expect("lock").http_mode = true;
                    }
                }
            }
        }
        {
            let mut st = n.inner.st.lock().expect("lock");
            st.ws = None;
            let o = st.client.disconnected();
            n.apply_locked(&mut st, o);
        }
        let fg = n.inner.conf.lock().expect("lock").foreground;
        let delay = reconnect_delay_ms(attempt, fg, rand::random::<f64>());
        attempt += 1;
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(delay)) => {}
            _ = n.inner.wake_transport.notified() => attempt = 0,
        }
    }
}

/// Runs one WebSocket session. Returns whether the server welcomed us (resets the backoff).
async fn run_socket(
    n: &Native,
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    token: &str,
) -> bool {
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    {
        let mut st = n.inner.st.lock().expect("lock");
        st.ws = Some(tx);
        st.http_mode = false;
        let o = st.client.connected(token, VERSION);
        n.apply_locked(&mut st, o);
    }
    let mut welcomed = false;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            m = stream.next() => match m {
                Some(Ok(Message::Binary(b))) => {
                    let Ok(msg) = proto::decode::<ServerMsg>(&b) else { continue };
                    welcomed |= matches!(msg, ServerMsg::Welcome(_));
                    let mut st = n.inner.st.lock().expect("lock");
                    let o = st.client.on_message(msg, now_ms());
                    n.apply_locked(&mut st, o);
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            f = rx.recv() => match f {
                Some(bytes) => {
                    if sink.send(Message::Binary(bytes.into())).await.is_err() {
                        break;
                    }
                }
                None => break, // the client declared the connection dead
            },
            _ = tick.tick() => {
                let mut st = n.inner.st.lock().expect("lock");
                let o = st.client.tick(now_ms());
                n.apply_locked(&mut st, o);
            }
            _ = n.inner.wake_transport.notified() => {
                // Online / resumed / token changed: probe now (a dead socket shows within seconds).
                if n.token().as_deref() != Some(token) {
                    break;
                }
                let ping = {
                    let mut st = n.inner.st.lock().expect("lock");
                    proto::encode(&st.client.probe(now_ms()))
                };
                if sink.send(Message::Binary(ping.into())).await.is_err() {
                    break;
                }
            }
        }
    }
    welcomed
}

/// The HTTP long-poll transport (`POST /api/sync`), used when WebSockets don't get through.
async fn http_loop(n: &Native, token: &str) {
    {
        let mut st = n.inner.st.lock().expect("lock");
        let o = st.client.connected(token, VERSION);
        n.apply_locked(&mut st, o);
    }
    loop {
        let (body, cursor_frames) = {
            let mut st = n.inner.st.lock().expect("lock");
            let frames = std::mem::take(&mut st.http_outbox);
            let mut hello = None;
            let mut ops = Vec::new();
            for f in &frames {
                match proto::decode::<ClientMsg>(f) {
                    Ok(ClientMsg::Hello(h)) => hello = Some(h),
                    Ok(ClientMsg::Push { ops: o }) => ops.extend(o),
                    _ => {}
                }
            }
            let Some(hello) = hello else { return };
            let wait_s = if ops.is_empty() { 20 } else { 0 };
            (
                proto::encode(&HttpSyncRequest {
                    cursor: st.client.cursor,
                    hello,
                    ops,
                    wait_s,
                }),
                frames,
            )
        };
        let url = match n.base() {
            Ok(b) => format!("{b}/api/sync"),
            Err(_) => return,
        };
        let r = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(40)))
                .build()
                .into();
            let mut resp = agent
                .post(&url)
                .header("content-type", "application/cbor")
                .send(&body[..])
                .map_err(|e| e.to_string())?;
            if resp.status().as_u16() != 200 {
                return Err(format!("sync {}", resp.status()));
            }
            resp.body_mut()
                .with_config()
                .limit(u64::MAX)
                .read_to_vec()
                .map_err(|e| e.to_string())
        })
        .await;
        let bytes = match r {
            Ok(Ok(b)) => b,
            _ => {
                // Keep what we meant to send; the caller backs off.
                n.inner
                    .st
                    .lock()
                    .expect("lock")
                    .http_outbox
                    .splice(0..0, cursor_frames);
                return;
            }
        };
        let Ok(resp) = proto::decode::<HttpSyncResponse>(&bytes) else {
            return;
        };
        {
            let mut st = n.inner.st.lock().expect("lock");
            let now = now_ms();
            let feed = |m: ServerMsg, st: &mut crate::State| {
                let o = st.client.on_message(m, now);
                n.apply_locked(st, o);
            };
            feed(ServerMsg::Welcome(resp.welcome), &mut st);
            if !resp.acks.is_empty() {
                feed(ServerMsg::Ack { results: resp.acks }, &mut st);
            }
            for ch in resp.changes {
                feed(ServerMsg::Changes(ch), &mut st);
            }
            // The next request carries a fresh hello (and whatever the client wants to push).
            let o = st.client.connected(token, VERSION);
            n.apply_locked(&mut st, o);
        }
        // Try WebSockets again now and then.
        if rand::random::<f64>() < 0.05 || n.token().as_deref() != Some(token) {
            n.inner.st.lock().expect("lock").http_mode = false;
            return;
        }
    }
}

// ------------------------------------------------------------------------ blob channel

async fn pump_loop(n: Native) {
    loop {
        let tasks = {
            let mut st = n.inner.st.lock().expect("lock");
            let mut t = st.client.blob_tasks();
            if let Some(p) = st.client.blobs.presence_task() {
                t.push(p);
            }
            t
        };
        if tasks.is_empty() || n.token().is_none() || n.server().is_none() {
            tokio::select! {
                _ = n.inner.wake_pump.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(10)) => {}
            }
            continue;
        }
        let mut handles = Vec::new();
        for t in tasks {
            let n2 = n.clone();
            handles.push(tokio::task::spawn_blocking(move || exec(&n2, t)));
        }
        let mut results = Vec::new();
        for h in handles {
            if let Ok(r) = h.await {
                results.push(r);
            }
        }
        let failed = results
            .iter()
            .filter(|r| matches!(r, BlobResult::Failed { .. }))
            .count();
        let all_failed = failed == results.len();
        {
            let mut st = n.inner.st.lock().expect("lock");
            let now = now_ms();
            let mut w = Vec::new();
            for r in results {
                w.extend(st.client.blob_result(r, now));
            }
            let o = jess_core::client::Output {
                writes: w,
                send: vec![],
                events: vec![jess_core::client::Event::StatusChanged],
            };
            n.apply_locked(&mut st, o);
        }
        if all_failed {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(120)))
        .build()
        .into()
}

/// Executes one blob transfer task (blocking HTTP).
fn exec(n: &Native, t: BlobTask) -> BlobResult {
    match exec_inner(n, &t) {
        Ok(r) => r,
        Err(_) => BlobResult::Failed { task: t },
    }
}

fn exec_inner(n: &Native, t: &BlobTask) -> Result<BlobResult, String> {
    let base = n.base()?;
    let auth = format!("Bearer {}", n.token().ok_or("no token")?);
    let a = agent();
    Ok(match t {
        BlobTask::Begin { hash, size } => {
            let mut r = a
                .post(&format!("{base}/api/blobs/{hash}/uploads"))
                .header("authorization", &auth)
                .send_json(json!({ "size": size }))
                .map_err(|e| e.to_string())?;
            if !r.status().is_success() {
                return Err(format!("begin {}", r.status()));
            }
            let v: Value = r.body_mut().read_json().map_err(|e| e.to_string())?;
            BlobResult::Began {
                hash: *hash,
                present: v["present"].as_bool().unwrap_or(false),
                upload_id: v["upload_id"].as_str().map(String::from),
                received: Bitmap(
                    hex::decode(v["received"].as_str().unwrap_or("")).unwrap_or_default(),
                ),
            }
        }
        BlobTask::PutChunk {
            hash,
            upload_id,
            index,
            offset,
            len,
        } => {
            let bytes = n
                .inner
                .blobs
                .read_at(hash, *offset, *len)
                .map_err(|e| e.to_string())?;
            let sum = hex::encode(Sha256::digest(&bytes));
            let r = a
                .put(&format!(
                    "{base}/api/blobs/{hash}/uploads/{upload_id}/chunks/{index}"
                ))
                .header("authorization", &auth)
                .header("x-chunk-sha256", &sum)
                .send(&bytes[..])
                .map_err(|e| e.to_string())?;
            match r.status().as_u16() {
                404 => BlobResult::UploadGone { hash: *hash },
                s if (200..300).contains(&s) => BlobResult::ChunkDone {
                    hash: *hash,
                    index: *index,
                    ok: true,
                },
                s => return Err(format!("put {s}")),
            }
        }
        BlobTask::Complete { hash, upload_id } => {
            let r = a
                .post(&format!(
                    "{base}/api/blobs/{hash}/uploads/{upload_id}/complete"
                ))
                .header("authorization", &auth)
                .send_empty()
                .map_err(|e| e.to_string())?;
            match r.status().as_u16() {
                404 => BlobResult::UploadGone { hash: *hash },
                422 => BlobResult::Completed {
                    hash: *hash,
                    ok: false,
                },
                s if (200..300).contains(&s) => BlobResult::Completed {
                    hash: *hash,
                    ok: true,
                },
                s => return Err(format!("complete {s}")),
            }
        }
        BlobTask::GetRange {
            hash,
            index,
            offset,
            len,
        } => {
            if *len == 0 {
                return Ok(BlobResult::RangeDone {
                    hash: *hash,
                    index: *index,
                    ok: false,
                });
            }
            let mut r = a
                .get(&format!("{base}/api/blobs/{hash}"))
                .header("authorization", &auth)
                .header("range", &format!("bytes={offset}-{}", offset + len - 1))
                .call()
                .map_err(|e| e.to_string())?;
            if !r.status().is_success() {
                return Ok(BlobResult::RangeDone {
                    hash: *hash,
                    index: *index,
                    ok: false,
                });
            }
            let body = r
                .body_mut()
                .with_config()
                .limit(u64::MAX)
                .read_to_vec()
                .map_err(|e| e.to_string())?;
            if body.len() as u64 != *len {
                return Ok(BlobResult::RangeDone {
                    hash: *hash,
                    index: *index,
                    ok: false,
                });
            }
            n.inner
                .blobs
                .write_at(hash, *offset, &body)
                .map_err(|e| e.to_string())?;
            BlobResult::RangeDone {
                hash: *hash,
                index: *index,
                ok: true,
            }
        }
        BlobTask::Presence { hashes } => {
            let mut r = a
                .post(&format!("{base}/api/blobs/presence"))
                .header("authorization", &auth)
                .send_json(json!({ "hashes": hashes.iter().map(Hash::to_hex).collect::<Vec<_>>() }))
                .map_err(|e| e.to_string())?;
            if !r.status().is_success() {
                return Err(format!("presence {}", r.status()));
            }
            let v: Value = r.body_mut().read_json().map_err(|e| e.to_string())?;
            let present: Vec<Hash> = v["present"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| Hash::parse_hex(x.as_str()?))
                        .collect()
                })
                .unwrap_or_default();
            let missing = hashes
                .iter()
                .filter(|h| !present.contains(h))
                .copied()
                .collect();
            BlobResult::Presence { present, missing }
        }
    })
}
