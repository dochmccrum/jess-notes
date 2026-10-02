//! Spaces (DESIGN §24): the registry, a local space served by the embedded server, and moving a
//! local space to a real server. Needs `--features spaces`.
#![cfg(feature = "spaces")]
use jess_native::spaces::{self, Embedded, Kind, Registry};
use jess_native::{EventSink, Native};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

const PASSWORD: &str = "correct horse battery";

fn now_ms() -> u64 {
    jess_native::now_ms()
}

fn new_id() -> String {
    let mut b = [0u8; 10];
    b[..8].copy_from_slice(&now_ms().to_le_bytes());
    b[8] = std::process::id() as u8;
    b[9] = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    jess_core::Id::new_v7(now_ms(), b).to_string()
}
static COUNTER: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

async fn until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(secs) {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

fn synced(n: &Native) -> bool {
    let s = n.status();
    s["state"] == "synced" && s["uploads"]["pending"] == 0 && s["downloads"] == 0
}

fn names(n: &Native) -> Vec<String> {
    n.init()["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|e| e.get("deleted").is_none())
        .filter_map(|e| e["name"].as_str().map(str::to_string))
        .collect()
}

/// Opens a local space the way the app does: embedded server, client pointed at it, signed in
/// with the space's secret on first use.
fn open_local(root: &Path, id: &str, secret: &str) -> (Embedded, Native) {
    let srv = Embedded::start(&spaces::server_dir(root, id), secret, false).unwrap();
    let sink: EventSink = Arc::new(|_| {});
    let n = Native::open(&spaces::data_dir(root, id), sink).unwrap();
    n.set_server(Some(srv.url.clone()));
    if n.token().is_none() {
        n.set_token(Some(
            spaces::sign_in(&srv.url, Some(secret), None, "this device").unwrap(),
        ));
    }
    (srv, n)
}

#[test]
fn registry_migrates_a_single_vault_and_saves_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Before spaces: one vault's client store in <root>/data.
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(root.join("data/marker"), b"x").unwrap();
    let r = Registry::load(root).unwrap();
    let s = r
        .active()
        .expect("the old vault became the active space")
        .clone();
    assert_eq!(s.kind, Kind::Remote);
    assert!(spaces::data_dir(root, &s.id).join("marker").exists());
    assert!(!root.join("data").exists());
    // Saved: loading again gives the same registry.
    let again = Registry::load(root).unwrap();
    assert_eq!(again.active.as_deref(), Some(s.id.as_str()));
    assert!(!root.join("spaces.json.tmp").exists());

    // A local space gets a secret; the open space can't be deleted, another one can.
    let mut r = again;
    let local = r.add(root, " Journal ", Kind::Local).unwrap().clone();
    assert_eq!(local.name, "Journal");
    assert!(local.secret.as_deref().is_some_and(|s| s.len() >= 32));
    assert!(r.public().to_string().contains("Journal"));
    assert!(!r
        .public()
        .to_string()
        .contains(local.secret.as_deref().unwrap()));
    assert!(r.remove(root, &s.id).is_err());
    r.remove(root, &local.id).unwrap();
    assert!(!spaces::space_dir(root, &local.id).exists());
    // A fresh device has no spaces at all.
    let fresh = tempfile::tempdir().unwrap();
    assert!(Registry::load(fresh.path()).unwrap().spaces.is_empty());
}

#[test]
fn parse_server_and_pairing_links() {
    assert_eq!(
        spaces::parse_server(" https://notes.example.com/ ").unwrap(),
        ("https://notes.example.com".into(), None)
    );
    assert_eq!(
        spaces::parse_server("https://notes.example.com/#pair=AB12-CD34").unwrap(),
        ("https://notes.example.com".into(), Some("AB12-CD34".into()))
    );
    assert!(spaces::parse_server("notes.example.com").is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_space_keeps_notes_across_restarts_and_moves_to_a_server() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("app");
    let mut reg = Registry::load(&root).unwrap();
    let space = reg.add(&root, "Journal", Kind::Local).unwrap().clone();
    reg.active = Some(space.id.clone());
    reg.save(&root).unwrap();
    let secret = space.secret.clone().unwrap();

    // Write a note and an attachment into the local space.
    let (srv, n) = open_local(&root, &space.id, &secret);
    assert!(srv.url.starts_with("http://127.0.0.1:"));
    until("local space synced", 20, || synced(&n)).await;
    let note = new_id();
    n.intent(&json!([{ "op": "create", "id": note, "kind": "markdown", "parent": null, "name": "Day one.md" }]).to_string())
        .unwrap();
    let d = jess_core::doc::new_doc(7);
    n.doc_update(
        &note,
        jess_core::doc::insert(&d, 0, "Wrote this offline, see [[Day two]]\n"),
    );
    let img = n
        .ingest_bytes("photo.png", b"\x89PNG\r\n\x1a\nnot really")
        .unwrap();
    let hash = img["hash"].as_str().unwrap().to_string();
    n.intent(&json!([{ "op": "create", "id": new_id(), "kind": "media", "parent": null, "name": "photo.png", "blob": hash }]).to_string())
        .unwrap();
    n.flush();
    until("local edits committed", 20, || synced(&n)).await;
    // Renames go through the same server rules (Invariant R): rewriting the link's target.
    let two = new_id();
    n.intent(&json!([{ "op": "create", "id": two, "kind": "markdown", "parent": null, "name": "Day two.md" }]).to_string())
        .unwrap();
    until("second note committed", 20, || synced(&n)).await;
    n.intent(&json!([{ "op": "setName", "id": two, "name": "Second day.md" }]).to_string())
        .unwrap();
    until("link rewritten by the local server", 20, || {
        n.doc_text(&note)
            .map(|t| t.contains("[[Second day]]"))
            .unwrap_or(false)
    })
    .await;

    // Restart (the app closing and opening): a new port, same data, signed in already.
    drop(n);
    drop(srv);
    let (srv2, n2) = open_local(&root, &space.id, &secret);
    until("reopened local space synced", 20, || synced(&n2)).await;
    assert!(names(&n2).contains(&"Day one.md".to_string()));
    assert_eq!(
        n2.doc_text(&note).unwrap(),
        "Wrote this offline, see [[Second day]]\n"
    );

    // Move it to a real server: export, upload, import there.
    let remote_dir = dir.path().join("remote");
    let remote = Embedded::start(&remote_dir, PASSWORD, false).unwrap();
    let (base, code) = spaces::parse_server(&remote.url).unwrap();
    assert!(code.is_none());
    let token = spaces::sign_in(&base, Some(PASSWORD), None, "mover").unwrap();
    assert!(spaces::sign_in(&base, Some("wrong"), None, "mover").is_err());
    let zip = dir.path().join("move.zip");
    let n3 = n2.clone();
    let z = zip.clone();
    tokio::task::spawn_blocking(move || n3.export_to(&z, true, true))
        .await
        .unwrap()
        .unwrap();
    let b = base.clone();
    let t = token.clone();
    let report: Value = tokio::task::spawn_blocking(move || spaces::import_zip(&b, &t, &zip))
        .await
        .unwrap()
        .unwrap();
    assert!(report.is_object(), "import report: {report}");

    // A device on the remote sees the notes and the attachment.
    let sink: EventSink = Arc::new(|_| {});
    let other = Native::open(&dir.path().join("other"), sink).unwrap();
    other.set_server(Some(base.clone()));
    other.set_token(Some(token));
    until("remote device sees the moved notes", 20, || {
        let ns = names(&other);
        ns.contains(&"Day one.md".to_string())
            && ns.contains(&"Second day.md".to_string())
            && ns.contains(&"photo.png".to_string())
    })
    .await;
    drop(other);
    drop(n2);
    drop(srv2);
    drop(remote);
}
