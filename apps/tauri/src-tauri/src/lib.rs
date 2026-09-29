//! The Tauri shell (DESIGN §2, §11.2): a thin IPC layer over `jess-native`, which runs the same
//! core as the web worker with SQLite, file blobs and a Rust transport. Command names and payload
//! shapes mirror `ui/src/backend/tauri.ts`. Binary payloads (doc updates, blob bytes) travel as
//! raw IPC bodies. `<img>`/PDF bytes are served by the `jess-blob` URI scheme (§7.7).
use jess_native::{io::Source, EventSink, Native};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::http::{header, Response as HttpResponse};
use tauri::ipc::{Channel, InvokeBody, Request, Response};
use tauri::{Manager, State};

type Events = Arc<Mutex<Option<Channel<Value>>>>;

struct App {
    native: Native,
    events: Events,
    /// The app's data directory; `ERASE` next to it asks the next launch to wipe it.
    root: PathBuf,
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

fn raw_body(req: &Request<'_>) -> Result<Vec<u8>, String> {
    match req.body() {
        InvokeBody::Raw(b) => Ok(b.clone()),
        InvokeBody::Json(_) => Err("expected a raw body".into()),
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
fn init(on_event: Channel<Value>, app: State<'_, App>) -> Value {
    // The channel first: nothing emitted after the snapshot below can be lost.
    *app.events.lock().expect("lock") = Some(on_event);
    app.native.init()
}

#[tauri::command]
fn get_server(app: State<'_, App>) -> Option<String> {
    app.native.server()
}

#[tauri::command]
fn set_server(url: Option<String>, app: State<'_, App>) {
    app.native.set_server(url)
}

#[tauri::command]
fn get_token(app: State<'_, App>) -> Option<String> {
    app.native.token()
}

#[tauri::command]
fn set_token(token: Option<String>, app: State<'_, App>) {
    app.native.set_token(token)
}

#[tauri::command]
fn meta_get(key: String, app: State<'_, App>) -> Option<String> {
    app.native.meta_get(&format!("ui:{key}"))
}

#[tauri::command]
fn meta_put(key: String, value: Option<String>, app: State<'_, App>) {
    app.native.meta_put(&format!("ui:{key}"), value.as_deref())
}

#[tauri::command]
async fn http_json(
    method: String,
    path: String,
    body: Option<Value>,
    auth: bool,
    app: State<'_, App>,
) -> Result<(u16, Value), String> {
    let n = app.native.clone();
    blocking(move || n.http_json(&method, &path, body, auth)).await
}

#[tauri::command]
fn quarantine(app: State<'_, App>) -> Value {
    app.native.quarantine()
}

#[tauri::command]
fn set_foreground(foreground: bool, app: State<'_, App>) {
    app.native.set_foreground(foreground)
}

#[tauri::command]
fn online(app: State<'_, App>) {
    app.native.online()
}

#[tauri::command]
fn intent(ops: String, app: State<'_, App>) -> Result<(), String> {
    app.native.intent(&ops)
}

/// Erasing local data (Settings → "Erase this device"): the stores are open, so the wipe happens
/// at the next launch, before anything opens them.
#[tauri::command]
fn request_erase(app: State<'_, App>, handle: tauri::AppHandle) -> Result<(), String> {
    app.native.flush();
    std::fs::write(app.root.join("ERASE"), b"1").map_err(|e| e.to_string())?;
    handle.restart()
}

// ------------------------------------------------------------------ docs

#[tauri::command]
fn open_doc(id: String, app: State<'_, App>) -> Result<Response, String> {
    Ok(Response::new(frame(&app.native.open_doc(&id)?)))
}

#[tauri::command]
fn close_doc(id: String, app: State<'_, App>) {
    app.native.close_doc(&id)
}

#[tauri::command]
fn doc_update(request: Request<'_>, app: State<'_, App>) -> Result<(), String> {
    let id = header_str(&request, "x-id");
    app.native.doc_update(&id, raw_body(&request)?);
    Ok(())
}

#[tauri::command]
fn flush(app: State<'_, App>) {
    app.native.flush()
}

#[tauri::command]
fn doc_text(id: String, app: State<'_, App>) -> Result<String, String> {
    app.native.doc_text(&id)
}

// ------------------------------------------------------------------ index

#[tauri::command]
async fn backlinks(id: String, app: State<'_, App>) -> Result<Vec<Value>, String> {
    Ok(app.native.backlinks(&id).await)
}

#[tauri::command]
async fn tags(app: State<'_, App>) -> Result<Vec<Value>, String> {
    Ok(app.native.tags().await)
}

#[tauri::command]
async fn notes_with_tag(tag: String, app: State<'_, App>) -> Result<Vec<String>, String> {
    Ok(app.native.notes_with_tag(&tag).await)
}

#[tauri::command]
async fn search(q: String, app: State<'_, App>) -> Result<Vec<Value>, String> {
    Ok(app.native.search(&q).await)
}

#[tauri::command]
async fn attachment_refs(app: State<'_, App>) -> Result<Value, String> {
    Ok(app.native.attachment_refs().await)
}

// ------------------------------------------------------------------ blobs

#[tauri::command]
async fn ingest(request: Request<'_>, app: State<'_, App>) -> Result<Value, String> {
    let name = header_str(&request, "x-name");
    let bytes = raw_body(&request)?;
    let n = app.native.clone();
    blocking(move || n.ingest_bytes(&name, &bytes)).await
}

#[tauri::command]
fn blob_want(hash: String, size: u64, prio: u8, app: State<'_, App>) {
    app.native.blob_want(&hash, size, prio)
}

#[tauri::command]
async fn blob_range(
    hash: String,
    begin: u64,
    end: u64,
    app: State<'_, App>,
) -> Result<Response, String> {
    let n = app.native.clone();
    Ok(Response::new(
        blocking(move || n.blob_range(&hash, begin, end)).await?,
    ))
}

#[tauri::command]
async fn blob_read(hash: String, variant: String, app: State<'_, App>) -> Result<Response, String> {
    let n = app.native.clone();
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
    app.native.set_offline_mode(everything)
}

// ------------------------------------------------------------------ import / export

#[tauri::command]
async fn import_plan(
    kind: String,
    path: String,
    hide_pdfs: bool,
    conflict: String,
    app: State<'_, App>,
) -> Result<Value, String> {
    let n = app.native.clone();
    let src = match kind.as_str() {
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
    let n = app.native.clone();
    blocking(move || n.import_run(&resolutions, apply_all.as_deref())).await
}

#[tauri::command]
fn import_cancel(app: State<'_, App>) {
    app.native.import_cancel()
}

#[tauri::command]
async fn export_to(
    path: String,
    portable: bool,
    zip: bool,
    app: State<'_, App>,
) -> Result<(), String> {
    let n = app.native.clone();
    blocking(move || n.export_to(Path::new(&path), portable, zip)).await
}

// ------------------------------------------------------------------ app

fn open_native(root: &Path, events: Events) -> Result<Native, String> {
    let data = root.join("data");
    if root.join("ERASE").exists() {
        if data.exists() {
            std::fs::remove_dir_all(&data).map_err(|e| format!("erase: {e}"))?;
        }
        let _ = std::fs::remove_file(root.join("ERASE"));
    }
    let sink: EventSink = Arc::new(move |v: Value| {
        if let Some(ch) = events.lock().expect("lock").as_ref() {
            let _ = ch.send(v);
        }
    });
    // Native starts its transport/pump tasks with tokio::spawn: open it inside the runtime.
    tauri::async_runtime::block_on(async move { Native::open(&data, sink) })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let root = app.path().app_data_dir()?;
            std::fs::create_dir_all(&root)?;
            let events: Events = Arc::new(Mutex::new(None));
            let native = open_native(&root, events.clone())?;
            app.manage(App {
                native,
                events,
                root,
            });
            Ok(())
        })
        .register_asynchronous_uri_scheme_protocol("jess-blob", |ctx, request, responder| {
            let native = ctx.app_handle().state::<App>().native.clone();
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
                window.state::<App>().native.flush();
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Jess Notes");
}
