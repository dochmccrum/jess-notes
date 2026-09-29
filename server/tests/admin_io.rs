//! Server-side zip import and streamed export over HTTP, and the live git mirror.

mod common;
use common::*;
use jess_core::Hash;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

fn zip_dir(root: &Path) -> Vec<u8> {
    let mut z = jess_core::zipstream::ZipStream::new(Vec::new());
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((p, rel)) = stack.pop() {
        for e in std::fs::read_dir(&p).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().to_string();
            let r = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if e.file_type().unwrap().is_dir() {
                stack.push((e.path(), r));
            } else {
                z.add_bytes(&r, &std::fs::read(e.path()).unwrap(), None, true)
                    .unwrap();
            }
        }
    }
    z.finish().unwrap()
}

fn upload(srv: &Server, auth: &str, bytes: &[u8]) -> Hash {
    let h = Hash::of(bytes);
    let v: serde_json::Value = ureq::post(&srv.url(&format!("/api/blobs/{h}/uploads")))
        .header("authorization", auth)
        .send_json(serde_json::json!({ "size": bytes.len() }))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    if v["present"] == true {
        return h;
    }
    let id = v["upload_id"].as_str().unwrap().to_string();
    let cs = v["chunk_size"].as_u64().unwrap() as usize;
    for (i, c) in bytes.chunks(cs).enumerate() {
        ureq::put(&srv.url(&format!("/api/blobs/{h}/uploads/{id}/chunks/{i}")))
            .header("authorization", auth)
            .header("x-chunk-sha256", &Hash::of(c).to_hex())
            .send(c)
            .unwrap();
    }
    ureq::post(&srv.url(&format!("/api/blobs/{h}/uploads/{id}/complete")))
        .header("authorization", auth)
        .send_empty()
        .unwrap();
    h
}

#[test]
fn zip_import_export_and_git_mirror() {
    let dir = tempfile::tempdir().unwrap();
    let srv = Server::start_env(
        dir.path(),
        free_port(),
        &[("JESS_GIT_COMMIT_INTERVAL", "1")],
    );
    let token = srv.login("admin");
    let auth = format!("Bearer {token}");
    let vault = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/vault");
    let h = upload(&srv, &auth, &zip_dir(&vault));
    let dry: serde_json::Value = ureq::post(&srv.url("/api/admin/import"))
        .header("authorization", &auth)
        .send_json(serde_json::json!({ "zip_hash": h.to_hex(), "dry_run": true }))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(dry["report"]["pdfs_hidden"], 1, "{dry}");
    assert!(dry["items"].as_array().unwrap().len() > 80);
    let rep: serde_json::Value = ureq::post(&srv.url("/api/admin/import"))
        .header("authorization", &auth)
        .send_json(serde_json::json!({ "zip_hash": h.to_hex() }))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(rep["rejected"].as_array().unwrap().len(), 0, "{rep}");
    // Streamed export equals the source (minus skipped items).
    let mut resp = ureq::get(&srv.url("/api/admin/export.zip"))
        .header("authorization", &auth)
        .call()
        .unwrap();
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/zip"
    );
    let z = resp
        .body_mut()
        .with_config()
        .limit(1 << 30)
        .read_to_vec()
        .unwrap();
    let mut a = zip::ZipArchive::new(std::io::Cursor::new(z)).unwrap();
    let mut got = BTreeMap::new();
    for i in 0..a.len() {
        let mut f = a.by_index(i).unwrap();
        if f.is_file() {
            let mut v = Vec::new();
            f.read_to_end(&mut v).unwrap();
            got.insert(f.name().to_string(), v);
        }
    }
    let mut want = BTreeMap::new();
    let mut stack = vec![(vault.clone(), String::new())];
    while let Some((p, rel)) = stack.pop() {
        for e in std::fs::read_dir(&p).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().to_string();
            let r = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if jess_core::import::skip_reason(&r).is_some() {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                stack.push((e.path(), r));
            } else {
                want.insert(r, std::fs::read(e.path()).unwrap());
            }
        }
    }
    assert_eq!(
        got.keys().collect::<Vec<_>>(),
        want.keys().collect::<Vec<_>>()
    );
    assert!(got == want, "export bytes differ");
    // The mirror catches up and git commits (attachments excluded).
    assert!(srv.wait_mirror(&token));
    let mirror = dir.path().join("mirror");
    let t = Instant::now();
    let mut log = String::new();
    while t.elapsed() < Duration::from_secs(20) {
        let o = std::process::Command::new("git")
            .current_dir(&mirror)
            .args(["log", "--format=%s"])
            .output();
        if let Ok(o) = o {
            log = String::from_utf8_lossy(&o.stdout).to_string();
            if log.starts_with("Jess: ") {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(log.contains("added"), "git log: {log:?}");
    let files = String::from_utf8_lossy(
        &std::process::Command::new("git")
            .current_dir(&mirror)
            .args(["ls-files"])
            .output()
            .unwrap()
            .stdout,
    )
    .to_string();
    assert!(!files.contains("shared.png") && files.contains("Welcome.md"));
    let status: serde_json::Value = ureq::get(&srv.url("/api/admin/status"))
        .header("authorization", &auth)
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert!(
        status["git"]["deploy_key"]
            .as_str()
            .map(|k| k.starts_with("ssh-ed25519"))
            .unwrap_or(true),
        "{status}"
    );
}
