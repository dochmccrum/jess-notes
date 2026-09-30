//! jess-native against a real server (in-process): two devices syncing notes and attachments,
//! a vault import from disk that exports back byte-for-byte, the `jess-blob` handler, and
//! foreground catch-up over a socket that died silently in the background.
use jess_core::import::{ImportSource, ZipSource};
use jess_native::{io::Source, EventSink, Native};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PASSWORD: &str = "correct horse battery";

fn now_ms() -> u64 {
    jess_native::now_ms()
}

/// Starts the server on its own thread and runtime; returns its base URL.
fn start_server(dir: &Path) -> String {
    let mut cfg = jess_server::config::Config::from_env();
    cfg.data_dir = dir.to_path_buf();
    cfg.ui_dir = dir.join("no-ui");
    cfg.admin_password = Some(PASSWORD.into());
    cfg.login_per_minute = 1000;
    cfg.git_enabled = false;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let app = jess_server::serve::build(&cfg).expect("build server");
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(l.local_addr().unwrap().port()).unwrap();
            let _ = jess_server::serve::run(app, l, std::future::pending(), false).await;
        });
    });
    let port = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("server port");
    format!("http://127.0.0.1:{port}")
}

fn login(url: &str, name: &str) -> String {
    let v: Value = ureq::post(&format!("{url}/api/auth/login"))
        .send_json(json!({ "password": PASSWORD, "device_name": name }))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    v["token"].as_str().unwrap().to_string()
}

fn device(dir: &Path, url: &str, name: &str) -> Native {
    let sink: EventSink = Arc::new(|_| {});
    let n = Native::open(dir, sink).unwrap();
    n.set_server(Some(url.into()));
    n.set_token(Some(login(url, name)));
    n
}

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

fn entries(n: &Native) -> Vec<Value> {
    n.init()["entries"].as_array().cloned().unwrap_or_default()
}

fn by_name(n: &Native, name: &str) -> Option<Value> {
    entries(n).into_iter().find(|e| e["name"] == name)
}

fn new_id() -> String {
    jess_core::Id::new_v7(now_ms(), rand_bytes()).to_string()
}

fn rand_bytes() -> [u8; 10] {
    let mut b = [0u8; 10];
    let x = now_ms().to_le_bytes();
    b[..8].copy_from_slice(&x);
    b[8] = std::process::id() as u8;
    b[9] =
        (std::process::id() >> 8) as u8 ^ COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    b
}
static COUNTER: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_devices_sync_notes_and_attachments() {
    let dir = tempfile::tempdir().unwrap();
    let url = start_server(&dir.path().join("server"));
    let a = device(&dir.path().join("a"), &url, "A");
    let b = device(&dir.path().join("b"), &url, "B");
    until("A synced", 10, || synced(&a)).await;

    // A note with CRLF text, created on A.
    let id = new_id();
    a.intent(&json!([{ "op": "create", "id": id, "kind": "markdown", "parent": null, "name": "Hello.md" }]).to_string())
        .unwrap();
    until("A synced the create", 10, || synced(&a)).await;
    let d = jess_core::doc::new_doc(7);
    a.doc_update(
        &id,
        jess_core::doc::insert(&d, 0, "Line one\r\nsee [[World]] #tag\r\n"),
    );
    // An edit still being coalesced isn't "synced" (a reload then would lose it).
    assert_eq!(a.status()["state"], "syncing");
    a.flush();
    until("B sees the note", 10, || by_name(&b, "Hello.md").is_some()).await;
    until("B has the text", 10, || {
        b.doc_text(&id)
            .map(|t| t == "Line one\r\nsee [[World]] #tag\r\n")
            .unwrap_or(false)
    })
    .await;
    // Its index works on B.
    until("B's tag index", 10, || {
        let n = b.clone();
        let tags = futures_block(async move { n.tags().await });
        tags.iter().any(|t| t["name"] == "tag")
    })
    .await;

    // An attachment ingested on A arrives on B, byte-for-byte, and is served with Range.
    let bytes: Vec<u8> = (0..(5u32 << 20) + 123)
        .map(|i| (i * 31 % 251) as u8)
        .collect();
    let info = a.ingest_bytes("data.bin", &bytes).unwrap();
    let hash = info["hash"].as_str().unwrap().to_string();
    let fid = new_id();
    a.intent(
        &json!([{ "op": "create", "id": fid, "kind": "media", "parent": null, "name": "data.bin", "blob": hash }])
            .to_string(),
    )
    .unwrap();
    until("A uploaded", 30, || synced(&a)).await;
    // Created without blobInfo: B learns the size from the server's blob row.
    let len = bytes.len() as u64;
    until("B knows the file's size", 10, || {
        by_name(&b, "data.bin").is_some_and(|e| e["blobInfo"]["size"] == len)
    })
    .await;
    let nb = b.clone();
    let h2 = hash.clone();
    let (status, headers, body) = tokio::task::spawn_blocking(move || {
        nb.serve_blob(&format!("/{h2}/orig"), Some("bytes=4194300-4194309"))
    })
    .await
    .unwrap();
    assert_eq!(status, 206);
    assert_eq!(body, bytes[4194300..4194310]);
    let hv: HashMap<_, _> = headers.into_iter().collect();
    assert_eq!(
        hv["content-range"],
        format!("bytes 4194300-4194309/{}", bytes.len())
    );
    // A whole read (streams from the server, and queues the download).
    let nb = b.clone();
    let h2 = hash.clone();
    let (status, _, body) =
        tokio::task::spawn_blocking(move || nb.serve_blob(&format!("/{h2}/orig"), None))
            .await
            .unwrap();
    assert_eq!(status, 200);
    assert!(body == bytes, "full body differs");
    until("B downloaded it", 30, || {
        b.blob_is_local(&jess_core::Hash::parse_hex(&hash).unwrap())
    })
    .await;

    // A rename on B rewrites A's link text through the server.
    let wid = new_id();
    b.intent(&json!([{ "op": "create", "id": wid, "kind": "markdown", "parent": null, "name": "World.md" }]).to_string())
        .unwrap();
    until("A sees World", 10, || by_name(&a, "World.md").is_some()).await;
    b.intent(&json!([{ "op": "setName", "id": wid, "name": "Earth.md" }]).to_string())
        .unwrap();
    until("A's link rewritten", 10, || {
        a.doc_text(&id)
            .map(|t| t.contains("[[Earth]]"))
            .unwrap_or(false)
    })
    .await;
}

/// A TCP proxy whose existing connections can be frozen: bytes stop flowing both ways but nothing
/// is closed, like the socket of a phone app that was frozen in the background or whose network
/// changed. New connections flow normally.
fn frozen_proxy(target: &str) -> (String, impl Fn()) {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicU64, Ordering};
    let target = target.trim_start_matches("http://").to_string();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    // Connections numbered below this are frozen.
    let frozen_below = Arc::new(AtomicU64::new(0));
    let accepted = Arc::new(AtomicU64::new(0));
    let fb = frozen_below.clone();
    let acc = accepted.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(c) = c else { continue };
            let n = acc.fetch_add(1, Ordering::SeqCst);
            let Ok(up) = std::net::TcpStream::connect(&target) else {
                continue;
            };
            for (mut from, mut to) in [(c.try_clone().unwrap(), up.try_clone().unwrap()), (up, c)] {
                let fb = fb.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 16384];
                    while let Ok(k) = from.read(&mut buf) {
                        if k == 0 {
                            break;
                        }
                        while n < fb.load(Ordering::SeqCst) {
                            std::thread::sleep(Duration::from_millis(20)); // swallowed, not closed
                        }
                        if to.write_all(&buf[..k]).is_err() {
                            break;
                        }
                    }
                    let _ = to.shutdown(std::net::Shutdown::Write);
                });
            }
        }
    });
    let freeze = move || frozen_below.store(accepted.load(Ordering::SeqCst), Ordering::SeqCst);
    (format!("http://127.0.0.1:{port}"), freeze)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreground_after_background_reconnects_instead_of_waiting_for_a_dead_socket() {
    let dir = tempfile::tempdir().unwrap();
    let url = start_server(&dir.path().join("server"));
    let (via, freeze) = frozen_proxy(&url);
    let a = device(&dir.path().join("a"), &url, "A");
    let b = device(&dir.path().join("b"), &via, "B");
    b.set_fresh_socket_after_ms(200);
    until("B synced", 10, || synced(&b)).await;

    // B goes to the background; its socket dies silently; A writes 200 notes meanwhile.
    b.set_foreground(false);
    freeze();
    let ops: Vec<Value> = (0..200)
        .map(|i| json!({ "op": "create", "id": new_id(), "kind": "markdown", "parent": null, "name": format!("Note {i}.md") }))
        .collect();
    a.intent(&Value::Array(ops).to_string()).unwrap();
    until("A synced", 10, || synced(&a)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = entries(&b).len();
    assert!(
        before < 200,
        "B shouldn't have received anything over the frozen socket"
    );

    // Back in the foreground: a fresh socket catches up well within the pong timeout (5 s).
    let t = Instant::now();
    b.set_foreground(true);
    until("B caught up", 10, || entries(&b).len() >= before + 200).await;
    let took = t.elapsed();
    assert!(took < Duration::from_secs(2), "catch-up took {took:?}");
    eprintln!("catch-up of 200 notes after a frozen socket: {took:?}");
}

/// Runs a small future to completion from a sync closure (tests only).
fn futures_block<T: Send + 'static>(f: impl std::future::Future<Output = T> + Send + 'static) -> T {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    })
    .join()
    .unwrap()
}

fn fixture_files(vault: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut want = BTreeMap::new();
    let mut stack = vec![(vault.to_path_buf(), String::new())];
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
    want
}

fn folder_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut got = BTreeMap::new();
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
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
                got.insert(r, std::fs::read(e.path()).unwrap());
            }
        }
    }
    got
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn import_from_disk_then_export_is_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let url = start_server(&dir.path().join("server"));
    let a = device(&dir.path().join("a"), &url, "A");
    until("A synced", 10, || synced(&a)).await;
    let vault = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/vault");
    let want = fixture_files(&vault);

    let n = a.clone();
    let v = vault.clone();
    let plan = tokio::task::spawn_blocking(move || n.import_plan(Source::Folder(v), true, "ask"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(plan["report"]["pdfs_hidden"], 1, "{}", plan["report"]);
    let n = a.clone();
    let rep = tokio::task::spawn_blocking(move || n.import_run(&HashMap::new(), None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rep["cancelled"], false);
    until("import uploaded", 60, || synced(&a)).await;

    // Zip export.
    let zip = dir.path().join("out.zip");
    let (n, z) = (a.clone(), zip.clone());
    tokio::task::spawn_blocking(move || n.export_to(&z, false, true))
        .await
        .unwrap()
        .unwrap();
    assert!(!dir.path().join("out.zip.partial").exists());
    let mut src = ZipSource::new(std::fs::File::open(&zip).unwrap()).unwrap();
    let (files, _) = src.list().unwrap();
    let mut got = BTreeMap::new();
    for f in files {
        got.insert(f.path.clone(), src.read_all(&f.path).unwrap());
    }
    assert_eq!(
        got.keys().collect::<Vec<_>>(),
        want.keys().collect::<Vec<_>>()
    );
    for (k, v) in &want {
        assert!(&got[k] == v, "{k} differs in the zip export");
    }

    // Folder export.
    let out = dir.path().join("out");
    let (n, o) = (a.clone(), out.clone());
    tokio::task::spawn_blocking(move || n.export_to(&o, false, false))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(folder_files(&out), want);

    // A second device gets the whole vault; its export matches too.
    let b = device(&dir.path().join("b"), &url, "B");
    until("B has every entry", 30, || {
        entries(&b).len() == entries(&a).len()
    })
    .await;
    let out_b = dir.path().join("out-b");
    let (n, o) = (b.clone(), out_b.clone());
    tokio::task::spawn_blocking(move || n.export_to(&o, false, false))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(folder_files(&out_b), want);

    // Search works over the imported notes.
    let hits = a.search("the").await;
    assert!(!hits.is_empty());

    // The zip export imports back into fresh devices: by path (desktop) and as an already-open
    // file (an Android `content://` document), with identical results.
    for (name, open_file) in [("C", false), ("D", true)] {
        let c = Native::open(&dir.path().join(name), Arc::new(|_| {})).unwrap();
        let src = if open_file {
            Source::ZipFile(Arc::new(std::fs::File::open(&zip).unwrap()))
        } else {
            Source::Zip(zip.clone())
        };
        let n = c.clone();
        tokio::task::spawn_blocking(move || n.import_plan(src, true, "ask"))
            .await
            .unwrap()
            .unwrap();
        let n = c.clone();
        let rep = tokio::task::spawn_blocking(move || n.import_run(&HashMap::new(), None))
            .await
            .unwrap()
            .unwrap_or_else(|e| panic!("{name}: zip import failed: {e}"));
        assert_eq!(rep["cancelled"], false);
        let out_c = dir.path().join(format!("out-{name}"));
        let (n, o) = (c.clone(), out_c.clone());
        tokio::task::spawn_blocking(move || n.export_to(&o, false, false))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(folder_files(&out_c), want, "{name}");
    }
}
