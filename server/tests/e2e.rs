//! End-to-end tests against the real `jess serve` process.

mod common;
use common::*;
use jess_core::blobs::{Bitmap, BlobResult, BlobTask};
use jess_core::model::{KIND_MARKDOWN, KIND_MEDIA};
use jess_core::ops::MetaOp;
use jess_core::{Hash, Id};
use std::time::{Duration, Instant};

fn create_note(id: Id, name: &str, parent: Option<Id>) -> MetaOp {
    MetaOp::Create {
        id,
        kind: KIND_MARKDOWN.into(),
        parent,
        name: name.into(),
        tree_visible: true,
        blob: None,
        blob_info: None,
        created_at: None,
        modified_at: None,
        props: vec![],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_two_clients_rename_rewrites_links_and_latency() {
    let dir = tempfile::tempdir().unwrap();
    let srv = Server::start(dir.path());
    let mut a = WsClient::new(srv.login("A"), 1);
    let mut b = WsClient::new(srv.login("B"), 2);
    a.connect(&srv.ws_url()).await;
    b.connect(&srv.ws_url()).await;
    assert!(
        a.until(Duration::from_secs(5), |c| c.status()
            == jess_core::client::Status::Synced)
            .await
    );
    let target = Id::new_v7(now_ms(), [1; 10]);
    let linker = Id::new_v7(now_ms(), [2; 10]);
    let o =
        a.c.local_meta(
            vec![
                create_note(target, "Old.md", None),
                create_note(linker, "Linker.md", None),
            ],
            now_ms(),
        )
        .unwrap();
    a.handle(o).await;
    let d = jess_core::doc::new_doc(11);
    let u = jess_core::doc::insert(&d, 0, "See [[Old]] and [x](Old.md#h).\r\n");
    let o = a.c.local_doc_update(linker, "body", u, now_ms());
    a.handle(o).await;
    assert!(
        b.until(Duration::from_secs(5), |c| c.view().get(&linker).is_some())
            .await
    );
    b.pump(Duration::from_millis(300)).await;
    assert_eq!(b.doc_text(linker), "See [[Old]] and [x](Old.md#h).\r\n");
    // Rename on B: the server rewrites A's links in the same transaction.
    let o =
        b.c.local_meta(
            vec![MetaOp::SetName {
                id: target,
                name: "New name.md".into(),
            }],
            now_ms(),
        )
        .unwrap();
    b.handle(o).await;
    // While offline-unconfirmed, B's redirects overlay still resolves [[Old]].
    let ok = a
        .until(Duration::from_secs(5), |c| {
            c.view()
                .get(&target)
                .map(|e| e.name == "New name.md")
                .unwrap_or(false)
        })
        .await;
    assert!(ok);
    a.pump(Duration::from_millis(300)).await;
    assert_eq!(
        a.doc_text(linker),
        "See [[New name]] and [x](New%20name.md#h).\r\n"
    );
    // Latency: edit on A → visible on B.
    let mut lat = Vec::new();
    for i in 0..20 {
        let before = b.remote_updates.len();
        let u = jess_core::doc::insert(&d, 0, &format!("{i}"));
        let t = Instant::now();
        let o = a.c.local_doc_update(linker, "body", u, now_ms());
        a.handle(o).await;
        let _ = a.pump(Duration::from_millis(1)).await;
        let got = b.until(Duration::from_secs(2), |_| false).await;
        let _ = got;
        let _ = before;
        lat.push(t.elapsed());
        if b.remote_updates.len() > before {
            let at = b.remote_updates.last().unwrap().2;
            *lat.last_mut().unwrap() = at - t;
        }
    }
    lat.sort();
    eprintln!(
        "edit→remote latency p50 {:?} max {:?}",
        lat[lat.len() / 2],
        lat.last().unwrap()
    );
    assert!(
        lat[lat.len() / 2] < Duration::from_millis(150),
        "p50 sync latency {:?}",
        lat[lat.len() / 2]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blobs_upload_resume_download_range() {
    let dir = tempfile::tempdir().unwrap();
    let srv = Server::start(dir.path());
    let token = srv.login("A");
    let bytes: Vec<u8> = (0..(9u32 << 20)).map(|i| (i * 31 % 251) as u8).collect();
    let h = Hash::of(&bytes);
    let mut a = WsClient::new(token.clone(), 5);
    a.connect(&srv.ws_url()).await;
    a.until(Duration::from_secs(5), |c| {
        c.conn == jess_core::client::Conn::Ready
    })
    .await;
    let w = a.c.blobs.ingest(h, bytes.len() as u64, now_ms());
    a.commit(w);
    let id = Id::new_v7(now_ms(), [9; 10]);
    let o =
        a.c.local_meta(
            vec![MetaOp::Create {
                id,
                kind: KIND_MEDIA.into(),
                parent: None,
                name: "big.bin".into(),
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
    let auth = format!("Bearer {token}");
    let mut dropped_once = false;
    for _ in 0..50 {
        let tasks = a.c.blob_tasks();
        if tasks.is_empty() {
            break;
        }
        for t in tasks {
            let r = match t.clone() {
                BlobTask::Begin { hash, size } => {
                    let mut resp = ureq::post(&srv.url(&format!("/api/blobs/{hash}/uploads")))
                        .header("authorization", &auth)
                        .send_json(serde_json::json!({ "size": size }))
                        .unwrap();
                    let v: serde_json::Value = resp.body_mut().read_json().unwrap();
                    BlobResult::Began {
                        hash,
                        present: v["present"].as_bool().unwrap(),
                        upload_id: v["upload_id"].as_str().map(String::from),
                        received: Bitmap(
                            hex::decode(v["received"].as_str().unwrap_or("")).unwrap(),
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
                    if index == 1 && !dropped_once {
                        dropped_once = true; // interrupted transfer: retried later
                        BlobResult::Failed { task: t }
                    } else {
                        let chunk = &bytes[offset as usize..(offset + len) as usize];
                        let r = ureq::put(&srv.url(&format!(
                            "/api/blobs/{hash}/uploads/{upload_id}/chunks/{index}"
                        )))
                        .header("authorization", &auth)
                        .header("x-chunk-sha256", &Hash::of(chunk).to_hex())
                        .send(chunk)
                        .unwrap();
                        BlobResult::ChunkDone {
                            hash,
                            index,
                            ok: r.status() == 200,
                        }
                    }
                }
                BlobTask::Complete { hash, upload_id } => {
                    let r = ureq::post(
                        &srv.url(&format!("/api/blobs/{hash}/uploads/{upload_id}/complete")),
                    )
                    .header("authorization", &auth)
                    .send_empty()
                    .unwrap();
                    BlobResult::Completed {
                        hash,
                        ok: r.status() == 200,
                    }
                }
                other => panic!("unexpected {other:?}"),
            };
            let w = a.c.blob_result(r, now_ms());
            a.commit(w);
        }
    }
    // Evictable once confirmed and the op referencing it is applied (DESIGN §22 item 57).
    assert!(
        a.c.pending_count() == 0 || !a.c.blobs.can_evict(&h),
        "held while its Create is pending"
    );
    a.until(Duration::from_secs(5), |c| c.pending_count() == 0)
        .await;
    assert!(a.c.blobs.can_evict(&h), "confirmed after upload");
    // Range download.
    let mut r = ureq::get(&srv.url(&format!("/api/blobs/{h}")))
        .header("authorization", &auth)
        .header("range", "bytes=4194300-4194309")
        .call()
        .unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(
        r.headers().get("cache-control").unwrap(),
        "private, max-age=31536000, immutable"
    );
    let got = r.body_mut().read_to_vec().unwrap();
    assert_eq!(got, bytes[4194300..4194310]);
    // Unauthenticated access is refused.
    assert!(ureq::get(&srv.url(&format!("/api/blobs/{h}")))
        .call()
        .is_err());
    // Dedupe: a second begin says present.
    let v: serde_json::Value = ureq::post(&srv.url(&format!("/api/blobs/{h}/uploads")))
        .header("authorization", &auth)
        .send_json(serde_json::json!({ "size": bytes.len() }))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(v["present"], true);
    let (ok, out) = integrity(dir.path(), true);
    assert!(ok, "{out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_revoke_and_http_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let srv = Server::start(dir.path());
    let token = srv.login("A");
    let v: serde_json::Value = ureq::post(&srv.url("/api/auth/pair"))
        .header("authorization", &format!("Bearer {token}"))
        .send_empty()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let code = v["code"].as_str().unwrap().to_string();
    let v: serde_json::Value = ureq::post(&srv.url("/api/auth/redeem"))
        .send_json(serde_json::json!({ "code": code, "device_name": "Phone" }))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let phone = v["token"].as_str().unwrap().to_string();
    let phone_id = v["device_id"].as_str().unwrap().to_string();
    assert!(
        ureq::post(&srv.url("/api/auth/redeem"))
            .send_json(serde_json::json!({ "code": code }))
            .is_err(),
        "single use"
    );
    // HTTP fallback sync with the paired token.
    let (mut c, _) = jess_core::client::Client::new(42, jess_core::blobs::CHUNK_SIZE);
    let hello = match c.connected(&phone, "t").send.remove(0) {
        jess_core::proto::ClientMsg::Hello(h) => h,
        _ => unreachable!(),
    };
    let id = Id::new_v7(now_ms(), [3; 10]);
    let o = c
        .local_meta(vec![create_note(id, "Via HTTP.md", None)], now_ms())
        .unwrap();
    let _ = o;
    let ops: Vec<_> = c.pending_ops().map(|p| p.op.clone()).collect();
    let req = jess_core::proto::HttpSyncRequest {
        hello,
        ops,
        cursor: 0,
        wait_s: 0,
    };
    let resp = ureq::post(&srv.url("/api/sync"))
        .send(&jess_core::proto::encode(&req)[..])
        .unwrap()
        .body_mut()
        .read_to_vec()
        .unwrap();
    let resp: jess_core::proto::HttpSyncResponse = jess_core::proto::decode(&resp).unwrap();
    assert!(matches!(
        resp.acks[0].1,
        jess_core::ops::AckResult::Applied(_)
    ));
    assert!(resp
        .changes
        .iter()
        .flat_map(|c| &c.entries)
        .any(|e| e.id == id));
    // Revoke the phone: its WebSocket session is closed with Revoked.
    let mut p = WsClient::new(phone.clone(), 43);
    p.connect(&srv.ws_url()).await;
    p.until(Duration::from_secs(5), |c| {
        c.conn == jess_core::client::Conn::Ready
    })
    .await;
    let r = ureq::post(&srv.url(&format!("/api/devices/{phone_id}/revoke")))
        .header("authorization", &format!("Bearer {token}"))
        .send_empty()
        .unwrap();
    assert_eq!(r.status(), 204);
    assert!(
        p.until(Duration::from_secs(5), |c| c.error.is_some()).await,
        "revoked device is disconnected"
    );
    assert!(ureq::get(&srv.url("/api/devices"))
        .header("authorization", &format!("Bearer {phone}"))
        .call()
        .is_err());
    // Wrong password is refused; rate limit kicks in.
    let mut limited = false;
    for _ in 0..8 {
        if let Err(ureq::Error::StatusCode(429)) = ureq::post(&srv.url("/api/auth/login"))
            .send_json(serde_json::json!({ "password": "nope" }))
        {
            limited = true
        }
    }
    assert!(limited);
}

/// Blob facts (size, mime, dimensions) reach every client with the blob row, and the creating
/// client knows them immediately from its own op.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blob_facts_propagate() {
    let dir = tempfile::tempdir().unwrap();
    let srv = Server::start(dir.path());
    let mut a = WsClient::new(srv.login("A"), 1);
    let mut b = WsClient::new(srv.login("B"), 2);
    a.connect(&srv.ws_url()).await;
    b.connect(&srv.ws_url()).await;
    for c in [&mut a, &mut b] {
        assert!(
            c.until(Duration::from_secs(5), |c| c.status()
                == jess_core::client::Status::Synced)
                .await
        );
    }
    let h = Hash::of(b"not really a png");
    let info = jess_core::model::BlobInfo {
        size: 16,
        mime: Some("image/png".into()),
        width: Some(640),
        height: Some(480),
        orientation: None,
    };
    let id = Id::new_v7(now_ms(), [7; 10]);
    let o =
        a.c.local_meta(
            vec![MetaOp::Create {
                id,
                kind: KIND_MEDIA.into(),
                parent: None,
                name: "pic.png".into(),
                tree_visible: false,
                blob: Some(h),
                blob_info: Some(info.clone()),
                created_at: None,
                modified_at: None,
                props: vec![],
            }],
            now_ms(),
        )
        .unwrap();
    assert_eq!(a.c.blob_facts(&h), Some(&info));
    a.handle(o).await;
    a.pump(Duration::from_millis(300)).await;
    let ok = b
        .until(Duration::from_secs(5), |c| {
            c.blob_facts(&h).and_then(|f| f.width) == Some(640)
        })
        .await;
    assert!(
        ok,
        "B never learned the facts: entry={:?} facts={:?} a_status={:?}",
        b.c.view().get(&id).map(|e| e.blob),
        b.c.blob_facts(&h),
        a.c.status()
    );
    assert_eq!(
        b.c.blob_facts(&h).unwrap().mime.as_deref(),
        Some("image/png")
    );
    // Facts survive a reload from the KV store.
    let (c2, _) = jess_core::client::Client::load(b.kv.clone(), 99, jess_core::blobs::CHUNK_SIZE);
    assert_eq!(c2.blob_facts(&h).and_then(|f| f.height), Some(480));
}

/// A signed-in device changes the vault password with the current one; the old one stops working,
/// the new one signs in, and other devices stay signed in.
#[test]
fn change_password() {
    let dir = tempfile::tempdir().unwrap();
    let srv = Server::start_env(
        dir.path(),
        free_port(),
        &[("JESS_LOGIN_RATE_PER_MINUTE", "1000")],
    );
    let token = srv.login("A");
    let other = srv.login("B");
    let change = |tok: &str, current: &str, new: &str| {
        ureq::post(&srv.url("/api/auth/password"))
            .header("authorization", &format!("Bearer {tok}"))
            .send_json(serde_json::json!({ "current": current, "new": new }))
    };
    assert!(matches!(
        change(&token, "wrong", "a new password"),
        Err(ureq::Error::StatusCode(401))
    ));
    assert!(matches!(
        change(&token, common::PASSWORD, "short"),
        Err(ureq::Error::StatusCode(400))
    ));
    assert!(matches!(
        change("not-a-token", common::PASSWORD, "a new password"),
        Err(ureq::Error::StatusCode(401))
    ));
    assert_eq!(
        change(&token, common::PASSWORD, "a new password")
            .unwrap()
            .status(),
        200
    );
    let login = |pw: &str| {
        ureq::post(&srv.url("/api/auth/login")).send_json(serde_json::json!({ "password": pw }))
    };
    assert!(login(common::PASSWORD).is_err());
    assert_eq!(login("a new password").unwrap().status(), 200);
    assert_eq!(
        ureq::get(&srv.url("/api/devices"))
            .header("authorization", &format!("Bearer {other}"))
            .call()
            .unwrap()
            .status(),
        200
    );
}
