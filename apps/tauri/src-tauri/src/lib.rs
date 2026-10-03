//! The Tauri shell (DESIGN §2, §11.2): a thin IPC layer over `jess-native`, which runs the same
//! core as the web worker with SQLite, file blobs and a Rust transport. Command names and payload
//! shapes mirror `ui/src/backend/tauri.ts`. Binary payloads (doc updates, blob bytes) travel as
//! raw IPC bodies. `<img>`/PDF bytes are served by the `jess-blob` URI scheme (§7.7).
use jess_native::spaces::{self, Kind, Registry};
use jess_native::{io::Source, EventSink, Native};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::http::{header, Response as HttpResponse};
use tauri::ipc::{Channel, InvokeBody, Request, Response};
use tauri::{Manager, State};

type Events = Arc<Mutex<Option<Channel<Value>>>>;

/// The app's identifier (tauri.conf.json): CEF's profile lives under it.
pub const APP_ID: &str = "app.jessnotes.notes";

/// Desktop Linux runs on CEF (120 Hz, DESIGN §23); elsewhere the system WebView through wry.
#[cfg(all(target_os = "linux", not(target_os = "android")))]
pub type Rt = tauri_runtime_cef::CefRuntime<tauri::EventLoopMessage>;
#[cfg(not(all(target_os = "linux", not(target_os = "android"))))]
pub type Rt = tauri::Wry;
type AppHandle = tauri::AppHandle<Rt>;

struct App {
    /// The open space's client, if a space is open (a fresh install has none yet).
    native: Option<Native>,
    events: Events,
    /// The app's data directory: `spaces.json` and `spaces/<id>/` (DESIGN §24).
    root: PathBuf,
    /// The open space (public fields only).
    space: Option<Value>,
    /// A local space's embedded server, stopped when the app exits.
    _embedded: Option<spaces::Embedded>,
}

impl App {
    fn n(&self) -> Result<&Native, String> {
        self.native
            .as_ref()
            .ok_or_else(|| "no space is open".to_string())
    }
}

/// Runs blocking work (HTTP, disk-heavy import/export) off the IPC and GUI threads.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

/// `[u32 LE length][bytes]…`
fn frame(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(parts.iter().map(|p| p.len() + 4).sum());
    for p in parts {
        out.extend_from_slice(&(p.len() as u32).to_le_bytes());
        out.extend_from_slice(p);
    }
    out
}

/// A binary IPC body. Android's IPC is JSON-only: there the UI sends `{b64}` (see `rawArg` in
/// `ui/src/backend/tauri.ts`); a plain Uint8Array would arrive as an array of numbers.
fn raw_body(req: &Request<'_>) -> Result<Vec<u8>, String> {
    use base64::Engine;
    match req.body() {
        InvokeBody::Raw(b) => Ok(b.clone()),
        InvokeBody::Json(Value::Object(o)) if o.get("b64").is_some_and(Value::is_string) => {
            base64::engine::general_purpose::STANDARD
                .decode(o["b64"].as_str().unwrap_or_default())
                .map_err(|e| format!("bad base64 body: {e}"))
        }
        InvokeBody::Json(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
            .collect::<Option<Vec<u8>>>()
            .ok_or_else(|| "expected a byte array".into()),
        InvokeBody::Json(_) => Err("expected a binary body".into()),
    }
}

fn header_str(req: &Request<'_>, name: &str) -> String {
    let v = req
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    percent_encoding::percent_decode_str(v)
        .decode_utf8_lossy()
        .into_owned()
}

// ------------------------------------------------------------------ session

#[tauri::command]
fn init(on_event: Channel<Value>, app: State<'_, App>) -> Result<Value, String> {
    // The channel first: nothing emitted after the snapshot below can be lost.
    *app.events.lock().expect("lock") = Some(on_event);
    Ok(app.n()?.init())
}

#[tauri::command]
fn get_server(app: State<'_, App>) -> Option<String> {
    app.native.as_ref().and_then(Native::server)
}

#[tauri::command]
fn set_server(url: Option<String>, app: State<'_, App>) -> Result<(), String> {
    app.n()?.set_server(url);
    Ok(())
}

#[tauri::command]
fn get_token(app: State<'_, App>) -> Option<String> {
    app.native.as_ref().and_then(Native::token)
}

#[tauri::command]
fn set_token(token: Option<String>, app: State<'_, App>) -> Result<(), String> {
    app.n()?.set_token(token);
    Ok(())
}

#[tauri::command]
fn meta_get(key: String, app: State<'_, App>) -> Option<String> {
    app.native.as_ref()?.meta_get(&format!("ui:{key}"))
}

#[tauri::command]
fn meta_put(key: String, value: Option<String>, app: State<'_, App>) -> Result<(), String> {
    app.n()?.meta_put(&format!("ui:{key}"), value.as_deref());
    Ok(())
}

#[tauri::command]
async fn http_json(
    method: String,
    path: String,
    body: Option<Value>,
    auth: bool,
    app: State<'_, App>,
) -> Result<(u16, Value), String> {
    let n = app.n()?.clone();
    blocking(move || n.http_json(&method, &path, body, auth)).await
}

#[tauri::command]
fn quarantine(app: State<'_, App>) -> Result<Value, String> {
    Ok(app.n()?.quarantine())
}

#[tauri::command]
fn set_foreground(foreground: bool, app: State<'_, App>) {
    if let Some(n) = &app.native {
        n.set_foreground(foreground)
    }
}

#[tauri::command]
fn online(app: State<'_, App>) {
    if let Some(n) = &app.native {
        n.online()
    }
}

#[tauri::command]
fn intent(ops: String, app: State<'_, App>) -> Result<(), String> {
    app.n()?.intent(&ops)
}

/// Erasing local data (Settings → "Erase this device"): the stores are open, so the wipe happens
/// at the next launch, before anything opens them.
#[tauri::command]
fn request_erase(app: State<'_, App>, handle: AppHandle) -> Result<(), String> {
    let space = app.space.as_ref().ok_or("no space is open")?;
    // A local space is the only copy: it's deleted (from another space), never "erased".
    if space["kind"] == "local" {
        return Err("a local space can't be erased: delete it from the spaces list".into());
    }
    let id = space["id"].as_str().unwrap_or_default();
    app.n()?.flush();
    std::fs::write(spaces::space_dir(&app.root, id).join("ERASE"), b"1")
        .map_err(|e| e.to_string())?;
    relaunch(&handle);
    Ok(())
}

// ------------------------------------------------------------------ spaces (DESIGN §24)

/// The open space, or null.
#[tauri::command]
fn space_current(app: State<'_, App>) -> Option<Value> {
    app.space.clone()
}

#[tauri::command]
fn spaces_list(app: State<'_, App>) -> Result<Value, String> {
    Ok(Registry::load(&app.root)?.public())
}

/// Adds a local space and opens it (the app restarts into it).
#[tauri::command]
fn space_add_local(name: String, app: State<'_, App>, handle: AppHandle) -> Result<(), String> {
    let mut reg = Registry::load(&app.root)?;
    let id = reg.add(&app.root, &name, Kind::Local)?.id.clone();
    reg.active = Some(id);
    reg.save(&app.root)?;
    switch_away(&app);
    relaunch(&handle);
    Ok(())
}

/// Signs in to a server (password, or the code in a pairing link), then adds the space and opens
/// it. Signing in first means a typo never leaves an empty space behind.
#[tauri::command]
async fn space_add_remote(
    name: Option<String>,
    server: String,
    password: Option<String>,
    device_name: String,
    app: State<'_, App>,
    handle: AppHandle,
) -> Result<(), String> {
    let (base, code) = spaces::parse_server(&server)?;
    let b = base.clone();
    let token =
        blocking(move || spaces::sign_in(&b, password.as_deref(), code.as_deref(), &device_name))
            .await?;
    let mut reg = Registry::load(&app.root)?;
    let host = base.split("://").nth(1).unwrap_or(&base).to_string();
    let s = reg.add(&app.root, name.as_deref().unwrap_or(&host), Kind::Remote)?;
    s.server = Some(base);
    s.pending_token = Some(token);
    let id = s.id.clone();
    reg.active = Some(id);
    reg.save(&app.root)?;
    switch_away(&app);
    relaunch(&handle);
    Ok(())
}

#[tauri::command]
fn space_switch(id: String, app: State<'_, App>, handle: AppHandle) -> Result<(), String> {
    let mut reg = Registry::load(&app.root)?;
    reg.get(&id).ok_or("no such space")?;
    if reg.active.as_deref() == Some(id.as_str()) {
        return Ok(());
    }
    reg.active = Some(id);
    reg.save(&app.root)?;
    switch_away(&app);
    relaunch(&handle);
    Ok(())
}

#[tauri::command]
fn space_rename(id: String, name: String, app: State<'_, App>) -> Result<Value, String> {
    let mut reg = Registry::load(&app.root)?;
    let name = name.trim();
    if name.is_empty() {
        return Err("a space needs a name".into());
    }
    reg.get_mut(&id).ok_or("no such space")?.name = name.into();
    reg.save(&app.root)?;
    Ok(reg.public())
}

/// Deletes a space and its data on this device (another space must be open).
#[tauri::command]
fn space_delete(id: String, app: State<'_, App>) -> Result<Value, String> {
    let mut reg = Registry::load(&app.root)?;
    reg.remove(&app.root, &id)?;
    reg.save(&app.root)?;
    Ok(reg.public())
}

/// Moves the open local space to a server: export, upload, import there; then adds that server
/// as a remote space and opens it. The local space stays, renamed "… (moved)" (DESIGN §24.2).
#[tauri::command]
async fn space_move_to_server(
    server: String,
    password: Option<String>,
    device_name: String,
    app: State<'_, App>,
    handle: AppHandle,
) -> Result<Value, String> {
    let space = app.space.clone().ok_or("no space is open")?;
    if space["kind"] != "local" {
        return Err("only a local space can be moved to a server".into());
    }
    let id = space["id"].as_str().unwrap_or_default().to_string();
    let name = space["name"].as_str().unwrap_or("Notes").to_string();
    let (base, code) = spaces::parse_server(&server)?;
    let n = app.n()?.clone();
    n.flush();
    let zip = spaces::space_dir(&app.root, &id).join("move.zip");
    let (b, z) = (base.clone(), zip.clone());
    let (token, report) = blocking(move || {
        let token = spaces::sign_in(&b, password.as_deref(), code.as_deref(), &device_name)?;
        n.export_to(&z, true, true)?;
        let report = spaces::import_zip(&b, &token, &z);
        let _ = std::fs::remove_file(&z);
        Ok((token, report?))
    })
    .await?;
    let mut reg = Registry::load(&app.root)?;
    if let Some(s) = reg.get_mut(&id) {
        s.name = format!("{name} (moved)");
    }
    let s = reg.add(&app.root, &name, Kind::Remote)?;
    s.server = Some(base);
    s.pending_token = Some(token);
    reg.active = Some(s.id.clone());
    reg.save(&app.root)?;
    switch_away(&app);
    let h = handle.clone();
    // Give the UI the report before the restart.
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        relaunch(&h);
    });
    Ok(report)
}

/// Restarts the app (into the space just made active). Tauri's `restart` starts the new process
/// while the old one is still running, which CEF can't share its profile and DevTools port with:
/// on desktop Linux the new process waits for this one to exit first (`main`, `JESS_WAIT_FOR_PID`).
fn relaunch(handle: &AppHandle) {
    // Android: the UI restarts the app through MainActivity (`JessAndroid.restart()`, a separate
    // process starts it again): Tauri's restart would re-execute a binary an APK doesn't have.
    #[cfg(target_os = "android")]
    {
        let _ = handle;
        return;
    }
    #[cfg(all(target_os = "linux", not(target_os = "android")))]
    if let Ok(exe) = std::env::current_exe() {
        use std::os::unix::process::CommandExt;
        let mut cmd = std::process::Command::new(exe);
        cmd.args(std::env::args_os().skip(1))
            .env("JESS_WAIT_FOR_PID", std::process::id().to_string());
        // Chromium's descriptors aren't all close-on-exec (the DevTools listening socket isn't):
        // inherited, they'd keep the port and the old instance's helpers alive in the new one.
        // SAFETY: only an async-signal-safe syscall between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32);
                Ok(())
            });
        }
        let spawned = cmd.spawn();
        if spawned.is_ok() {
            // Asks the event loop to stop (commands run on it: never block here). CEF's shutdown
            // sometimes takes longer than the new process waits (or hangs): edits are flushed and
            // committed already (`switch_away`), so after 3 s this process ends regardless.
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(3));
                std::process::exit(0);
            });
            handle.exit(0);
            return;
        }
    }
    #[cfg(not(target_os = "android"))]
    handle.restart()
}

/// Before a restart into another space: persist coalesced edits.
fn switch_away(app: &App) {
    if let Some(n) = &app.native {
        n.flush();
    }
}

// ------------------------------------------------------------------ docs

#[tauri::command]
fn open_doc(id: String, app: State<'_, App>) -> Result<Response, String> {
    Ok(Response::new(frame(&app.n()?.open_doc(&id)?)))
}

#[tauri::command]
fn close_doc(id: String, app: State<'_, App>) {
    if let Some(n) = &app.native {
        n.close_doc(&id)
    }
}

#[tauri::command]
fn doc_update(request: Request<'_>, app: State<'_, App>) -> Result<(), String> {
    let id = header_str(&request, "x-id");
    app.n()?.doc_update(&id, raw_body(&request)?);
    Ok(())
}

#[tauri::command]
fn flush(app: State<'_, App>) {
    if let Some(n) = &app.native {
        n.flush()
    }
}

#[tauri::command]
fn doc_text(id: String, app: State<'_, App>) -> Result<String, String> {
    app.n()?.doc_text(&id)
}

// ------------------------------------------------------------------ index

#[tauri::command]
async fn backlinks(id: String, app: State<'_, App>) -> Result<Vec<Value>, String> {
    Ok(app.n()?.backlinks(&id).await)
}

#[tauri::command]
async fn tags(app: State<'_, App>) -> Result<Vec<Value>, String> {
    Ok(app.n()?.tags().await)
}

#[tauri::command]
async fn notes_with_tag(tag: String, app: State<'_, App>) -> Result<Vec<String>, String> {
    Ok(app.n()?.notes_with_tag(&tag).await)
}

#[tauri::command]
async fn search(q: String, app: State<'_, App>) -> Result<Vec<Value>, String> {
    Ok(app.n()?.search(&q).await)
}

#[tauri::command]
async fn attachment_refs(app: State<'_, App>) -> Result<Value, String> {
    Ok(app.n()?.attachment_refs().await)
}

// ------------------------------------------------------------------ blobs

#[tauri::command]
async fn ingest(request: Request<'_>, app: State<'_, App>) -> Result<Value, String> {
    let name = header_str(&request, "x-name");
    let bytes = raw_body(&request)?;
    let n = app.n()?.clone();
    blocking(move || n.ingest_bytes(&name, &bytes)).await
}

#[tauri::command]
fn blob_want(hash: String, size: u64, prio: u8, app: State<'_, App>) {
    if let Some(n) = &app.native {
        n.blob_want(&hash, size, prio)
    }
}

#[tauri::command]
async fn blob_range(
    hash: String,
    begin: u64,
    end: u64,
    app: State<'_, App>,
) -> Result<Response, String> {
    let n = app.n()?.clone();
    Ok(Response::new(
        blocking(move || n.blob_range(&hash, begin, end)).await?,
    ))
}

#[tauri::command]
async fn blob_read(hash: String, variant: String, app: State<'_, App>) -> Result<Response, String> {
    let n = app.n()?.clone();
    let r = blocking(move || n.blob_read(&hash, &variant)).await?;
    // [mime length u32][mime][bytes]; empty = unavailable.
    Ok(Response::new(match r {
        None => vec![],
        Some((bytes, mime)) => {
            let m = mime.unwrap_or_default().into_bytes();
            let mut out = Vec::with_capacity(4 + m.len() + bytes.len());
            out.extend_from_slice(&(m.len() as u32).to_le_bytes());
            out.extend_from_slice(&m);
            out.extend_from_slice(&bytes);
            out
        }
    }))
}

#[tauri::command]
fn set_offline_mode(everything: bool, app: State<'_, App>) {
    if let Some(n) = &app.native {
        n.set_offline_mode(everything)
    }
}

// ------------------------------------------------------------------ import / export

#[tauri::command]
async fn import_plan(
    kind: String,
    path: String,
    hide_pdfs: bool,
    conflict: String,
    app: State<'_, App>,
    handle: AppHandle,
) -> Result<Value, String> {
    let n = app.n()?.clone();
    let src = match kind.as_str() {
        "zip" if is_content_uri(&path) => {
            Source::ZipFile(Arc::new(open_document(&handle, &path, false)?))
        }
        "zip" => Source::Zip(path.into()),
        _ => Source::Folder(path.into()),
    };
    blocking(move || n.import_plan(src, hide_pdfs, &conflict)).await
}

#[tauri::command]
async fn import_run(
    resolutions: HashMap<String, String>,
    apply_all: Option<String>,
    app: State<'_, App>,
) -> Result<Value, String> {
    let n = app.n()?.clone();
    blocking(move || n.import_run(&resolutions, apply_all.as_deref())).await
}

#[tauri::command]
fn import_cancel(app: State<'_, App>) {
    if let Some(n) = &app.native {
        n.import_cancel()
    }
}

#[tauri::command]
async fn export_to(
    path: String,
    portable: bool,
    zip: bool,
    app: State<'_, App>,
    handle: AppHandle,
) -> Result<(), String> {
    let n = app.n()?.clone();
    if zip && is_content_uri(&path) {
        let f = open_document(&handle, &path, true)?;
        return blocking(move || n.export_zip_into(f, portable)).await;
    }
    blocking(move || n.export_to(Path::new(&path), portable, zip)).await
}

/// Android's pickers return `content://` document URIs, not paths.
fn is_content_uri(path: &str) -> bool {
    path.starts_with("content://")
}

/// Opens a picked document through the fs plugin (a `content://` URI becomes a descriptor from the
/// content resolver). Zips are read with seeks: a provider that only hands out a pipe (some cloud
/// providers) is copied to the cache directory first.
fn open_document(handle: &AppHandle, uri: &str, write: bool) -> Result<std::fs::File, String> {
    use std::io::Seek;
    use tauri_plugin_fs::{FilePath, FsExt, OpenOptions};
    let url = uri.parse().map_err(|e| format!("{uri}: {e}"))?;
    let mut opts = OpenOptions::new();
    if write {
        opts.write(true).truncate(true).create(true);
    } else {
        opts.read(true);
    }
    let mut f = handle
        .fs()
        .open(FilePath::Url(url), opts)
        .map_err(|e| format!("couldn't open the document: {e}"))?;
    if write || f.stream_position().is_ok() && f.seek(std::io::SeekFrom::End(0)).is_ok() {
        if !write {
            f.rewind().map_err(|e| e.to_string())?;
        }
        return Ok(f);
    }
    let dir = handle.path().app_cache_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join("import.zip");
    let mut out = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    std::io::copy(&mut f, &mut out).map_err(|e| e.to_string())?;
    out.rewind().map_err(|e| e.to_string())?;
    // The open descriptor keeps the data; nothing is left behind in the cache.
    let _ = std::fs::remove_file(&tmp);
    Ok(out)
}

// ------------------------------------------------------------------ app

/// The open space's client, its public description, and a local space's server.
type Opened = (Option<Native>, Option<Value>, Option<spaces::Embedded>);

/// Opens the active space (DESIGN §24.1): its client store, and for a local space the embedded
/// server it syncs with. A fresh install has no space: the UI offers to add one.
fn open_space(root: &Path, events: Events) -> Result<Opened, String> {
    let mut reg = Registry::load(root)?;
    let Some(space) = reg.active().cloned() else {
        return Ok((None, None, None));
    };
    let dir = spaces::space_dir(root, &space.id);
    let data = dir.join("data");
    if dir.join("ERASE").exists() {
        if data.exists() {
            std::fs::remove_dir_all(&data).map_err(|e| format!("erase: {e}"))?;
        }
        let _ = std::fs::remove_file(dir.join("ERASE"));
    }
    let sink: EventSink = Arc::new(move |v: Value| {
        if let Some(ch) = events.lock().expect("lock").as_ref() {
            let _ = ch.send(v);
        }
    });
    // Native starts its transport/pump tasks with tokio::spawn: open it inside the runtime.
    let native = tauri::async_runtime::block_on(async move { Native::open(&data, sink) })?;
    let mut embedded = None;
    match space.kind {
        Kind::Local => {
            let secret = space
                .secret
                .clone()
                .ok_or("the local space has no secret")?;
            // Derivation re-executes this binary (`main` answers `derive`): not on Android.
            let srv = spaces::Embedded::start(
                &spaces::server_dir(root, &space.id),
                &secret,
                cfg!(all(target_os = "linux", not(target_os = "android"))),
            )?;
            // A new port every launch; the token from the first sign-in stays valid.
            native.set_server(Some(srv.url.clone()));
            if native.token().is_none() {
                let url = srv.url.clone();
                let t = std::thread::spawn(move || {
                    spaces::sign_in(&url, Some(&secret), None, "this device")
                })
                .join()
                .map_err(|_| "signing in to the local space failed".to_string())??;
                native.set_token(Some(t));
            }
            embedded = Some(srv);
        }
        Kind::Remote => {
            if let Some(token) = space.pending_token.clone() {
                native.set_server(space.server.clone());
                native.set_token(Some(token));
                if let Some(s) = reg.get_mut(&space.id) {
                    s.pending_token = None;
                }
                reg.save(root)?;
            } else if space.server.is_none() {
                // A space migrated from before spaces: learn its server from the client.
                if let Some(url) = native.server() {
                    if let Some(s) = reg.get_mut(&space.id) {
                        if s.name == "My notes" {
                            s.name = url.split("://").nth(1).unwrap_or(&url).to_string();
                        }
                        s.server = Some(url);
                    }
                    reg.save(root)?;
                }
            }
        }
    }
    let public = reg.active().map(spaces::Space::public);
    Ok((Some(native), public, embedded))
}

/// CEF re-executes this binary for its renderer, GPU and utility processes, and each must register
/// the same custom schemes. Call first thing in `main`: true means this process was such a helper
/// and has finished.
#[cfg(all(target_os = "linux", not(target_os = "android")))]
pub fn cef_helper() -> bool {
    let mut command_line_args = vec![("password-store".into(), Some("basic".into()))];
    // The desktop smoke test drives the app over the DevTools protocol (WebKitWebDriver can't).
    if let Ok(port) = std::env::var("JESS_CDP_PORT") {
        command_line_args.push(("remote-debugging-port".into(), Some(port)));
    }
    tauri_runtime_cef::configure(tauri_runtime_cef::CefConfig {
        identifier: APP_ID.into(),
        command_line_args,
        custom_schemes: vec![
            "tauri".into(),
            "ipc".into(),
            "asset".into(),
            "jess-blob".into(),
        ],
        ..Default::default()
    });
    if std::env::args().any(|a| a.starts_with("--type=")) {
        tauri_runtime_cef::run_cef_helper_process();
        return true;
    }
    false
}

/// Starts this process again with `--no-sandbox` on its command line when the sandbox can't work
/// (see [`appimage_without_sandbox`]). Chromium decides on the sandbox from the process's own
/// arguments, before the runtime's command-line hook runs, so the switch has to be there. Call
/// first thing in `main` (not in CEF's helper processes, which inherit the switch).
#[cfg(all(target_os = "linux", not(target_os = "android")))]
pub fn ensure_no_sandbox_arg() {
    use std::os::unix::process::CommandExt;
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args
        .iter()
        .any(|a| a == "--no-sandbox" || a.to_string_lossy().starts_with("--type="))
        || !appimage_without_sandbox()
    {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        let e = std::process::Command::new(exe)
            .args(&args[1..])
            .arg("--no-sandbox")
            .exec();
        eprintln!("jess: couldn't restart without the sandbox: {e}");
    }
}

/// The AppImage on a system that restricts unprivileged user namespaces (Ubuntu 23.10+ through
/// AppArmor, some Debian kernels by sysctl): Chromium's sandbox needs them, and only an installed
/// AppArmor profile (the .deb's) can grant them. There the AppImage runs without the sandbox
/// rather than not at all (owner, 2026-10-04; DESIGN §23.2); the .deb always keeps it.
#[cfg(all(target_os = "linux", not(target_os = "android")))]
pub fn appimage_without_sandbox() -> bool {
    // An explicit escape hatch, any package: `JESS_NO_SANDBOX=1`.
    if std::env::var_os("JESS_NO_SANDBOX").is_some() {
        return true;
    }
    if std::env::var_os("APPIMAGE").is_none() {
        return false;
    }
    let sysctl = |p: &str| {
        std::fs::read_to_string(p)
            .map(|v| v.trim().to_string())
            .ok()
    };
    let restricted = sysctl("/proc/sys/kernel/apparmor_restrict_unprivileged_userns").as_deref()
        == Some("1")
        || sysctl("/proc/sys/kernel/unprivileged_userns_clone").as_deref() == Some("0")
        || sysctl("/proc/sys/user/max_user_namespaces").as_deref() == Some("0");
    if restricted {
        eprintln!("jess: this system restricts user namespaces, so the AppImage runs without Chromium's sandbox (install the .deb to keep it)");
    }
    restricted
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::<Rt>::new()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|_app| {
            #[cfg(target_os = "android")]
            _app.handle().plugin(tauri_plugin_barcode_scanner::init())?;
            Ok(())
        })
        .setup(|app| {
            let root = app.path().app_data_dir()?;
            std::fs::create_dir_all(&root)?;
            let events: Events = Arc::new(Mutex::new(None));
            let (native, space, embedded) = open_space(&root, events.clone())?;
            app.manage(App {
                native,
                events,
                root,
                space,
                _embedded: embedded,
            });
            Ok(())
        })
        .register_asynchronous_uri_scheme_protocol("jess-blob", |ctx, request, responder| {
            let Some(native) = ctx.app_handle().state::<App>().native.clone() else {
                responder.respond(
                    HttpResponse::builder()
                        .status(404)
                        .body(Vec::new())
                        .expect("response"),
                );
                return;
            };
            let path = request.uri().path().to_string();
            let range = request
                .headers()
                .get(header::RANGE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            tauri::async_runtime::spawn_blocking(move || {
                let (status, headers, body) = native.serve_blob(&path, range.as_deref());
                let mut r = HttpResponse::builder()
                    .status(status)
                    .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
                for (k, v) in headers {
                    r = r.header(k, v);
                }
                responder.respond(r.body(body).expect("response"));
            });
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // Persist coalesced edits before the window goes.
                if let Some(n) = &window.state::<App>().native {
                    n.flush();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            init,
            get_server,
            set_server,
            get_token,
            set_token,
            meta_get,
            meta_put,
            http_json,
            quarantine,
            set_foreground,
            online,
            intent,
            request_erase,
            open_doc,
            close_doc,
            doc_update,
            flush,
            doc_text,
            backlinks,
            tags,
            notes_with_tag,
            search,
            attachment_refs,
            ingest,
            blob_want,
            blob_range,
            blob_read,
            set_offline_mode,
            import_plan,
            import_run,
            import_cancel,
            export_to,
            spaces_list,
            space_add_local,
            space_add_remote,
            space_switch,
            space_rename,
            space_delete,
            space_move_to_server,
            space_current,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Jess Notes");
}
