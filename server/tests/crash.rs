//! Process-level crash safety (DESIGN §17.3): drive load against `jess serve`, SIGKILL it at a
//! random point (mid-push, mid-chunk), restart, run `integrity-check --hashes`, and check that
//! every acknowledged write survived. `CRASH_ITERS` (default 6).

mod common;
use common::*;
use jess_core::blobs::{Bitmap, BlobResult, BlobTask};
use jess_core::model::{KIND_MARKDOWN, KIND_MEDIA};
use jess_core::ops::{AckResult, MetaOp};
use jess_core::{Hash, Id};
use std::collections::HashMap;
use std::time::Duration;

fn blob_io(srv_url: &str, auth: &str, bytes: &HashMap<Hash, Vec<u8>>, t: BlobTask) -> BlobResult {
    let res = (|| -> Result<BlobResult, ureq::Error> {
        Ok(match t.clone() {
            BlobTask::Begin { hash, size } => {
                let v: serde_json::Value =
                    ureq::post(&format!("{srv_url}/api/blobs/{hash}/uploads"))
                        .header("authorization", auth)
                        .send_json(serde_json::json!({ "size": size }))?
                        .body_mut()
                        .read_json()?;
                BlobResult::Began {
                    hash,
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
                let chunk = &bytes[&hash][offset as usize..(offset + len) as usize];
                match ureq::put(&format!(
                    "{srv_url}/api/blobs/{hash}/uploads/{upload_id}/chunks/{index}"
                ))
                .header("authorization", auth)
                .header("x-chunk-sha256", &Hash::of(chunk).to_hex())
                .send(chunk)
                {
                    Ok(_) => BlobResult::ChunkDone {
                        hash,
                        index,
                        ok: true,
                    },
                    Err(ureq::Error::StatusCode(404)) => BlobResult::UploadGone { hash },
                    Err(e) => return Err(e),
                }
            }
            BlobTask::Complete { hash, upload_id } => match ureq::post(&format!(
                "{srv_url}/api/blobs/{hash}/uploads/{upload_id}/complete"
            ))
            .header("authorization", auth)
            .send_empty()
            {
                Ok(_) => BlobResult::Completed { hash, ok: true },
                Err(ureq::Error::StatusCode(404)) => BlobResult::UploadGone { hash },
                Err(ureq::Error::StatusCode(422)) => BlobResult::Completed { hash, ok: false },
                Err(e) => return Err(e),
            },
            other => panic!("{other:?}"),
        })
    })();
    res.unwrap_or(BlobResult::Failed { task: t })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kill9_during_load() {
    let iters: usize = std::env::var("CRASH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6);
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let mut srv = Server::start_on(dir.path(), port);
    let token = srv.login("crash");
    let auth = format!("Bearer {token}");
    let url = srv.url("");
    let mut a = WsClient::new(token.clone(), 77);
    let mut blobs: HashMap<Hash, Vec<u8>> = HashMap::new();
    let mut seed = 12345u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut docs: Vec<(Id, yrs::Doc)> = Vec::new();
    let mut acked_entries: Vec<Id> = Vec::new();
    for it in 0..iters {
        a.connect(&srv.ws_url()).await;
        a.until(Duration::from_secs(5), |c| {
            c.conn == jess_core::client::Conn::Ready
        })
        .await;
        let kill_after = Duration::from_millis(150 + rnd() % 1200);
        let start = std::time::Instant::now();
        while start.elapsed() < kill_after {
            match rnd() % 5 {
                0 => {
                    let id = Id::new_v7(now_ms(), [(rnd() % 255) as u8; 10]);
                    let o =
                        a.c.local_meta(
                            vec![MetaOp::Create {
                                id,
                                kind: KIND_MARKDOWN.into(),
                                parent: None,
                                name: format!("n{}.md", rnd() % 50),
                                tree_visible: true,
                                blob: None,
                                blob_info: None,
                                created_at: None,
                                modified_at: None,
                                props: vec![],
                            }],
                            now_ms(),
                        )
                        .unwrap();
                    a.handle(o).await;
                    docs.push((id, jess_core::doc::new_doc(rnd() & 0xffff_ffff)));
                }
                1 if it % 2 == 0 => {
                    // A multi-chunk blob so kills land mid-chunk.
                    let size = (4 << 20) + (rnd() % (5 << 20)) as usize;
                    let b: Vec<u8> = (0..size)
                        .map(|i| (i as u64 * 7 + it as u64) as u8)
                        .collect();
                    let h = Hash::of(&b);
                    blobs.insert(h, b);
                    let w = a.c.blobs.ingest(h, size as u64, now_ms());
                    a.commit(w);
                    let id = Id::new_v7(now_ms(), [(rnd() % 255) as u8; 10]);
                    let o =
                        a.c.local_meta(
                            vec![MetaOp::Create {
                                id,
                                kind: KIND_MEDIA.into(),
                                parent: None,
                                name: format!("b{}.bin", rnd() % 1000),
                                tree_visible: false,
                                blob: Some(h),
                                blob_info: None,
                                created_at: None,
                                modified_at: None,
                                props: vec![],
                            }],
                            now_ms(),
                        )
                        .unwrap();
                    a.handle(o).await;
                }
                _ => {
                    if let Some((id, d)) = docs.last() {
                        let u = jess_core::doc::insert(
                            d,
                            0,
                            &format!("line {} [[n{}]]\n", rnd() % 1000, rnd() % 50),
                        );
                        let o = a.c.local_doc_update(*id, "body", u, now_ms());
                        a.handle(o).await;
                    }
                }
            }
            // Blob work in a blocking thread (so kills land mid-transfer).
            let tasks = a.c.blob_tasks();
            if !tasks.is_empty() {
                let (u, au, bl) = (url.clone(), auth.clone(), blobs.clone());
                let results = tokio::task::spawn_blocking(move || {
                    tasks
                        .into_iter()
                        .map(|t| blob_io(&u, &au, &bl, t))
                        .collect::<Vec<_>>()
                })
                .await
                .unwrap();
                for r in results {
                    let w = a.c.blob_result(r, now_ms());
                    a.commit(w);
                }
            }
            a.pump(Duration::from_millis(5)).await;
        }
        srv.kill9();
        a.pump(Duration::from_millis(50)).await;
        let o = a.c.disconnected();
        a.commit(o.writes);
        let (ok, out) = integrity(dir.path(), true);
        assert!(
            ok,
            "iteration {it}: integrity-check failed after SIGKILL:\n{out}"
        );
        srv = Server::start_on(dir.path(), port);
    }
    // Final: reconnect, drain everything, then a fresh replica must see the same state.
    a.connect(&srv.ws_url()).await;
    for _ in 0..200 {
        let tasks = a.c.blob_tasks();
        let (u, au, bl) = (url.clone(), auth.clone(), blobs.clone());
        let results = tokio::task::spawn_blocking(move || {
            tasks
                .into_iter()
                .map(|t| blob_io(&u, &au, &bl, t))
                .collect::<Vec<_>>()
        })
        .await
        .unwrap();
        for r in results {
            let w = a.c.blob_result(r, now_ms());
            a.commit(w);
        }
        a.pump(Duration::from_millis(30)).await;
        if a.c.pending_count() == 0
            && a.c.blobs.unconfirmed().is_empty()
            && a.c.status() == jess_core::client::Status::Synced
        {
            break;
        }
    }
    assert_eq!(a.c.pending_count(), 0);
    assert!(a.c.blobs.unconfirmed().is_empty());
    for q in a.c.quarantine() {
        panic!("unexpected rejection {:?}", q);
    }
    let _ = AckResult::Applied(0);
    let mut fresh = WsClient::new(token.clone(), 78);
    fresh.connect(&srv.ws_url()).await;
    let head = a.c.cursor;
    assert!(
        fresh
            .until(Duration::from_secs(10), |c| c.cursor >= head)
            .await
    );
    for (id, _) in &docs {
        assert_eq!(
            fresh.doc_text(*id),
            a.doc_text(*id),
            "doc {id} differs after crashes"
        );
        acked_entries.push(*id);
    }
    let (ok, out) = integrity(dir.path(), true);
    assert!(ok, "{out}");
    srv.stop();
}
