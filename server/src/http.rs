//! HTTP + WebSocket API (DESIGN §5.3, §7, §14, §15).

use crate::auth::{self, RateLimiter};
use crate::blobfs::BlobFs;
use crate::changes::read_changes;
use crate::config::{now_ms, Config};
use crate::db;
use crate::engine::BeginUpload;
use crate::writer::Writer;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path as AxPath, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use jess_core::proto::{
    self, ClientMsg, ErrorCode, Hello, HttpSyncRequest, HttpSyncResponse, ServerMsg, Welcome,
    CHANGES_PAGE_BYTES, PROTO_VERSION,
};
use jess_core::{Hash, Id};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::broadcast;

pub const MAX_WS_FRAME: usize = 32 << 20;

pub struct App {
    pub cfg: Config,
    pub writer: Writer,
    pub fs: BlobFs,
    pub vault_id: Id,
    pub tokens: RwLock<HashMap<[u8; 32], Id>>,
    pub revoked: broadcast::Sender<Id>,
    pub limiter: Mutex<RateLimiter>,
    pub setup_code: Mutex<Option<String>>,
    pub started: u64,
    pub status: Mutex<serde_json::Map<String, serde_json::Value>>,
}

pub type Shared = Arc<App>;

impl App {
    pub fn reader(&self) -> rusqlite::Result<rusqlite::Connection> {
        db::open_readonly(&self.cfg.db_path())
    }

    pub fn device(&self, headers: &HeaderMap) -> Option<Id> {
        let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
        let t = v.strip_prefix("Bearer ")?.trim();
        self.device_for_token(t)
    }

    pub fn device_for_token(&self, token: &str) -> Option<Id> {
        self.tokens
            .read()
            .ok()?
            .get(&auth::token_hash(token))
            .copied()
    }

    pub fn set_status(&self, key: &str, v: serde_json::Value) {
        if let Ok(mut s) = self.status.lock() {
            s.insert(key.to_string(), v);
        }
    }
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

fn unauthorized() -> Response {
    err(StatusCode::UNAUTHORIZED, "unauthorized")
}

fn client_ip(app: &App, headers: &HeaderMap, peer: SocketAddr) -> IpAddr {
    if app.cfg.trust_proxy {
        if let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|s| s.trim().parse().ok())
        {
            return ip;
        }
    }
    peer.ip()
}

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/auth/state", get(auth_state))
        .route("/api/auth/setup", post(auth_setup))
        .route("/api/auth/login", post(auth_login))
        .route("/api/auth/pair", post(auth_pair))
        .route("/api/auth/redeem", post(auth_redeem))
        .route("/api/auth/logout", post(auth_logout))
        .route("/api/devices", get(devices_list))
        .route("/api/devices/{id}/revoke", post(devices_revoke))
        .route("/api/sync", get(sync_ws).post(sync_http))
        .route("/api/blobs/presence", post(blob_presence))
        .route("/api/blobs/{hash}", get(blob_get).head(blob_get))
        .route("/api/blobs/{hash}/uploads", post(upload_begin))
        .route(
            "/api/blobs/{hash}/uploads/{id}/chunks/{index}",
            put(upload_chunk),
        )
        .route(
            "/api/blobs/{hash}/uploads/{id}/complete",
            post(upload_complete),
        )
        .route("/api/blobs/{hash}/derived/{kind}", get(blob_derived))
        .route("/api/admin/status", get(admin_status))
        .route("/api/admin/snapshot/latest", get(admin_snapshot))
        .fallback(static_files)
        .layer(DefaultBodyLimit::max(
            (jess_core::blobs::CHUNK_SIZE + 64 * 1024) as usize,
        ))
        .layer(axum::middleware::from_fn(security_headers))
        .with_state(app)
}

async fn security_headers(req: Request<Body>, next: axum::middleware::Next) -> Response {
    let mut r = next.run(req).await;
    let h = r.headers_mut();
    h.insert(
        "strict-transport-security",
        HeaderValue::from_static("max-age=31536000"),
    );
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    if !h.contains_key("content-security-policy") {
        h.insert(
            "content-security-policy",
            HeaderValue::from_static("default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' blob: data: https:; font-src 'self' data:; connect-src 'self'; worker-src 'self' blob:; object-src 'none'; frame-ancestors 'none'; base-uri 'none'"),
        );
    }
    r
}

// ---------------------------------------------------------------- health / admin

async fn healthz(State(app): State<Shared>) -> Response {
    let r = tokio::time::timeout(
        Duration::from_secs(3),
        app.writer.call(|e| {
            e.conn
                .query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
                .is_ok()
        }),
    )
    .await;
    match r {
        Ok(true) => (StatusCode::OK, "ok").into_response(),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "database unavailable").into_response(),
    }
}

async fn admin_status(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if app.device(&headers).is_none() {
        return unauthorized();
    }
    let facts = app
        .writer
        .call(|e| {
            let q = |sql: &str| e.conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0);
            json!({
                "vault_id": e.vault_id.to_string(),
                "head_seq": e.head,
                "entries": q("SELECT count(*) FROM entries WHERE purged = 0 AND kind != 'vault'"),
                "trashed": q("SELECT count(*) FROM entries WHERE purged = 0 AND trashed_batch IS NOT NULL"),
                "doc_rows": q("SELECT count(*) FROM doc_updates"),
                "blobs": q("SELECT count(*) FROM blobs WHERE present = 1"),
                "blob_bytes": q("SELECT ifnull(sum(size), 0) FROM blobs WHERE present = 1"),
                "uploads_in_progress": q("SELECT count(*) FROM uploads"),
                "missing_blobs": e.missing_blobs().map(|v| v.iter().map(|h| h.to_hex()).collect::<Vec<_>>()).unwrap_or_default(),
            })
        })
        .await;
    let mut out = facts.as_object().cloned().unwrap_or_default();
    out.insert("version".into(), json!(env!("CARGO_PKG_VERSION")));
    out.insert("uptime_s".into(), json!((now_ms() - app.started) / 1000));
    out.insert(
        "snapshots".into(),
        json!(crate::snapshot::list(&app.cfg.snapshots_dir())
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect::<Vec<_>>()),
    );
    if let Ok(s) = app.status.lock() {
        for (k, v) in s.iter() {
            out.insert(k.clone(), v.clone());
        }
    }
    if let Ok(c) = app.reader() {
        out.insert(
            "devices".into(),
            json!(auth::list_devices(&c).unwrap_or_default()),
        );
    }
    Json(serde_json::Value::Object(out)).into_response()
}

async fn admin_snapshot(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if app.device(&headers).is_none() {
        return unauthorized();
    }
    let Some(p) = crate::snapshot::list(&app.cfg.snapshots_dir()).pop() else {
        return err(StatusCode::NOT_FOUND, "no snapshot yet");
    };
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    match tokio::fs::File::open(&p).await {
        Ok(f) => {
            let body = Body::from_stream(tokio_util::io::ReaderStream::new(f));
            Response::builder()
                .header(header::CONTENT_TYPE, "application/zstd")
                .header(
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{name}\""),
                )
                .body(body)
                .expect("response")
        }
        Err(_) => err(StatusCode::NOT_FOUND, "snapshot unreadable"),
    }
}

// ---------------------------------------------------------------- auth

async fn auth_state(State(app): State<Shared>) -> Response {
    let needs_setup = app
        .writer
        .call(|e| auth::account_hash(&e.conn).ok().flatten().is_none())
        .await;
    Json(json!({ "needs_setup": needs_setup, "setup_code_required": needs_setup && app.setup_code.lock().map(|s| s.is_some()).unwrap_or(true), "vault_id": app.vault_id.to_string() })).into_response()
}

#[derive(Deserialize)]
struct SetupReq {
    password: String,
    setup_code: Option<String>,
    device_name: Option<String>,
}

async fn issue_token(app: &Shared, name: String) -> Response {
    let token = auth::random_token();
    let t2 = token.clone();
    let now = now_ms();
    match app
        .writer
        .call(move |e| auth::create_device(&e.conn, &name, &t2, now))
        .await
    {
        Ok(id) => {
            app.tokens
                .write()
                .expect("lock")
                .insert(auth::token_hash(&token), id);
            Json(json!({ "token": token, "device_id": id.to_string(), "vault_id": app.vault_id.to_string() })).into_response()
        }
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "could not create device"),
    }
}

async fn auth_setup(
    State(app): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SetupReq>,
) -> Response {
    let now = now_ms();
    let ip = client_ip(&app, &headers, peer);
    if let Err(ms) = app.limiter.lock().expect("lock").check(ip, now) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", ((ms / 1000) + 1).to_string())],
            "too many attempts",
        )
            .into_response();
    }
    if req.password.chars().count() < 8 {
        return err(
            StatusCode::BAD_REQUEST,
            "password must be at least 8 characters",
        );
    }
    let expected = app.setup_code.lock().expect("lock").clone();
    match (&expected, &req.setup_code) {
        (Some(code), Some(given))
            if constant_eq(
                code.replace('-', "").to_uppercase().as_bytes(),
                given.replace('-', "").trim().to_uppercase().as_bytes(),
            ) => {}
        (None, _) => {}
        _ => {
            app.limiter.lock().expect("lock").failed(now);
            return err(StatusCode::FORBIDDEN, "invalid setup code");
        }
    }
    let pw = req.password.clone();
    let phc = tokio::task::spawn_blocking(move || auth::hash_password(&pw))
        .await
        .expect("hash");
    let created = app
        .writer
        .call(move |e| {
            if auth::account_hash(&e.conn).ok().flatten().is_some() {
                return false;
            }
            auth::set_password(&e.conn, &phc, false, now).is_ok()
        })
        .await;
    if !created {
        return err(StatusCode::CONFLICT, "already set up");
    }
    *app.setup_code.lock().expect("lock") = None;
    issue_token(
        &app,
        req.device_name.unwrap_or_else(|| "First device".into()),
    )
    .await
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Deserialize)]
struct LoginReq {
    password: String,
    device_name: Option<String>,
}

async fn auth_login(
    State(app): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> Response {
    let now = now_ms();
    let ip = client_ip(&app, &headers, peer);
    if let Err(ms) = app.limiter.lock().expect("lock").check(ip, now) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", ((ms / 1000) + 1).to_string())],
            "too many attempts",
        )
            .into_response();
    }
    let phc = app
        .writer
        .call(|e| auth::account_hash(&e.conn).ok().flatten())
        .await;
    // Always run argon2 (constant-ish time whether or not an account exists).
    let pw = req.password.clone();
    let ok = tokio::task::spawn_blocking(move || match phc {
        Some(h) => auth::verify_password(&pw, &h),
        None => {
            let _ = auth::hash_password(&pw);
            false
        }
    })
    .await
    .unwrap_or(false);
    if !ok {
        app.limiter.lock().expect("lock").failed(now);
        return err(StatusCode::UNAUTHORIZED, "wrong password");
    }
    issue_token(&app, req.device_name.unwrap_or_else(|| "Device".into())).await
}

async fn auth_pair(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if app.device(&headers).is_none() {
        return unauthorized();
    }
    let now = now_ms();
    match app
        .writer
        .call(move |e| auth::create_pairing(&e.conn, now))
        .await
    {
        Ok(code) => {
            Json(json!({ "code": code, "expires_at": now + auth::PAIRING_TTL_MS })).into_response()
        }
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "could not create code"),
    }
}

#[derive(Deserialize)]
struct RedeemReq {
    code: String,
    device_name: Option<String>,
}

async fn auth_redeem(
    State(app): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<RedeemReq>,
) -> Response {
    let now = now_ms();
    let ip = client_ip(&app, &headers, peer);
    if let Err(ms) = app.limiter.lock().expect("lock").check(ip, now) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", ((ms / 1000) + 1).to_string())],
            "too many attempts",
        )
            .into_response();
    }
    let code = req.code.trim().to_lowercase();
    let ok = app
        .writer
        .call(move |e| auth::redeem_pairing(&e.conn, &code, now).unwrap_or(false))
        .await;
    if !ok {
        app.limiter.lock().expect("lock").failed(now);
        return err(StatusCode::FORBIDDEN, "invalid or expired code");
    }
    issue_token(
        &app,
        req.device_name.unwrap_or_else(|| "Paired device".into()),
    )
    .await
}

async fn revoke(app: &Shared, id: Id) -> bool {
    let now = now_ms();
    let ok = app
        .writer
        .call(move |e| auth::revoke_device(&e.conn, id, now).unwrap_or(false))
        .await;
    if ok {
        app.tokens.write().expect("lock").retain(|_, d| *d != id);
        let _ = app.revoked.send(id);
    }
    ok
}

async fn auth_logout(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(d) = app.device(&headers) else {
        return unauthorized();
    };
    revoke(&app, d).await;
    StatusCode::NO_CONTENT.into_response()
}

async fn devices_list(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if app.device(&headers).is_none() {
        return unauthorized();
    }
    let v = app
        .writer
        .call(|e| auth::list_devices(&e.conn).unwrap_or_default())
        .await;
    Json(v).into_response()
}

async fn devices_revoke(
    State(app): State<Shared>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
) -> Response {
    if app.device(&headers).is_none() {
        return unauthorized();
    }
    let Some(id) = Id::parse(&id) else {
        return err(StatusCode::BAD_REQUEST, "bad id");
    };
    if revoke(&app, id).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        err(StatusCode::NOT_FOUND, "no such device")
    }
}

// ---------------------------------------------------------------- sync

fn welcome(app: &App) -> Welcome {
    Welcome {
        vault_id: app.vault_id,
        head_seq: *app.writer.head.borrow(),
        server_time: now_ms(),
        min_client_proto: PROTO_VERSION,
        hlc: *app.writer.hlc.borrow(),
    }
}

fn check_hello(app: &App, h: &Hello) -> Result<Id, (ErrorCode, &'static str)> {
    if h.proto < PROTO_VERSION {
        return Err((ErrorCode::ProtoTooOld, "client protocol too old"));
    }
    let Some(d) = app.device_for_token(&h.token) else {
        return Err((ErrorCode::Unauthorized, "invalid token"));
    };
    if let Some(v) = h.vault_id {
        if v != app.vault_id {
            return Err((
                ErrorCode::WrongVault,
                "this device's local data belongs to a different vault",
            ));
        }
    }
    Ok(d)
}

async fn sync_ws(State(app): State<Shared>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(MAX_WS_FRAME)
        .on_upgrade(move |s| ws_session(app, s))
}

async fn send(ws: &mut WebSocket, m: &ServerMsg) -> bool {
    ws.send(Message::Binary(proto::encode(m).into()))
        .await
        .is_ok()
}

/// Sends pages until the connection's cursor reaches the head.
async fn pump(
    app: &Shared,
    ws: &mut WebSocket,
    conn: &Arc<Mutex<rusqlite::Connection>>,
    cursor: &mut u64,
    replica: u64,
) -> bool {
    loop {
        let head = *app.writer.head.borrow();
        if *cursor >= head {
            return true;
        }
        let (c2, from, hlc) = (conn.clone(), *cursor, *app.writer.hlc.borrow());
        let r = tokio::task::spawn_blocking(move || {
            let c = c2.lock().expect("lock");
            read_changes(&c, from, head, replica, CHANGES_PAGE_BYTES, hlc)
        })
        .await;
        match r {
            Ok(Ok(ch)) => {
                *cursor = ch.to;
                if !send(ws, &ServerMsg::Changes(ch)).await {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

async fn ws_session(app: Shared, mut ws: WebSocket) {
    // The first frame must be Hello, within 5 s (tokens never go in URLs).
    let first = tokio::time::timeout(Duration::from_secs(5), ws.recv()).await;
    let hello = match first {
        Ok(Some(Ok(Message::Binary(b)))) => match proto::decode::<ClientMsg>(&b) {
            Ok(ClientMsg::Hello(h)) => h,
            _ => return,
        },
        _ => return,
    };
    let device = match check_hello(&app, &hello) {
        Ok(d) => d,
        Err((code, message)) => {
            send(
                &mut ws,
                &ServerMsg::Error {
                    code,
                    message: message.into(),
                    retry_after: None,
                },
            )
            .await;
            return;
        }
    };
    let Ok(rconn) = app.reader() else { return };
    let rconn = Arc::new(Mutex::new(rconn));
    let replica = hello.replica_id;
    let mut cursor = hello.cursor;
    let now = now_ms();
    app.writer
        .call(move |e| auth::touch_device(&e.conn, device, now))
        .await
        .ok();
    if !send(&mut ws, &ServerMsg::Welcome(welcome(&app))).await {
        return;
    }
    let mut head_rx = app.writer.head.clone();
    let mut revoked = app.revoked.subscribe();
    let mut ping = tokio::time::interval(Duration::from_secs(20));
    let mut last_heard = tokio::time::Instant::now();
    if !pump(&app, &mut ws, &rconn, &mut cursor, replica).await {
        return;
    }
    loop {
        tokio::select! {
            m = ws.recv() => {
                let Some(Ok(m)) = m else { break };
                last_heard = tokio::time::Instant::now();
                let bytes = match m {
                    Message::Binary(b) => b,
                    Message::Close(_) => break,
                    _ => continue,
                };
                let msg = match proto::decode::<ClientMsg>(&bytes) {
                    Ok(m) => m,
                    Err(_) => {
                        send(&mut ws, &ServerMsg::Error { code: ErrorCode::BadFrame, message: "bad frame".into(), retry_after: None }).await;
                        break;
                    }
                };
                match msg {
                    ClientMsg::Hello(_) => {}
                    ClientMsg::Push { ops } => {
                        let now = now_ms();
                        let r = app.writer.call(move |e| e.push(replica, Some(device), &ops, now).map_err(|e| e.to_string())).await;
                        match r {
                            Ok(results) => {
                                if !send(&mut ws, &ServerMsg::Ack { results }).await { break; }
                            }
                            Err(e) => {
                                tracing::error!("push failed: {e}");
                                send(&mut ws, &ServerMsg::Error { code: ErrorCode::Internal, message: "push failed; retry".into(), retry_after: Some(1000) }).await;
                                break;
                            }
                        }
                        if !pump(&app, &mut ws, &rconn, &mut cursor, replica).await { break; }
                    }
                    ClientMsg::Pull { from, .. } => {
                        cursor = from;
                        if !pump(&app, &mut ws, &rconn, &mut cursor, replica).await { break; }
                    }
                    ClientMsg::Ping { nonce } => {
                        if !send(&mut ws, &ServerMsg::Pong { nonce, server_time: now_ms() }).await { break; }
                    }
                    ClientMsg::DocSync { entry, slot, state_vector } => {
                        let r = app.writer.call(move |e| e.doc_diff(entry, &slot, &state_vector).ok().flatten().map(|u| (slot, u))).await;
                        if let Some((slot, update)) = r {
                            if !send(&mut ws, &ServerMsg::DocDiff { entry, slot, update }).await { break; }
                        }
                    }
                }
            }
            r = head_rx.changed() => {
                if r.is_err() { break; }
                if !pump(&app, &mut ws, &rconn, &mut cursor, replica).await { break; }
            }
            r = revoked.recv() => {
                if let Ok(id) = r {
                    if id == device {
                        send(&mut ws, &ServerMsg::Error { code: ErrorCode::Revoked, message: "device revoked".into(), retry_after: None }).await;
                        break;
                    }
                }
            }
            _ = ping.tick() => {
                if last_heard.elapsed() > Duration::from_secs(45) { break; }
                if ws.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
            }
        }
    }
    let _ = ws.send(Message::Close(None)).await;
}

/// HTTP long-poll fallback (`POST /api/sync`, CBOR bodies).
async fn sync_http(State(app): State<Shared>, body: axum::body::Bytes) -> Response {
    let Ok(req) = proto::decode::<HttpSyncRequest>(&body) else {
        return err(StatusCode::BAD_REQUEST, "bad request");
    };
    let device = match check_hello(&app, &req.hello) {
        Ok(d) => d,
        Err((code, m)) => {
            return (
                StatusCode::UNAUTHORIZED,
                proto::encode(&ServerMsg::Error {
                    code,
                    message: m.into(),
                    retry_after: None,
                }),
            )
                .into_response()
        }
    };
    let replica = req.hello.replica_id;
    let mut acks = Vec::new();
    if !req.ops.is_empty() {
        let ops = req.ops;
        let now = now_ms();
        match app
            .writer
            .call(move |e| {
                e.push(replica, Some(device), &ops, now)
                    .map_err(|e| e.to_string())
            })
            .await
        {
            Ok(a) => acks = a,
            Err(_) => return err(StatusCode::SERVICE_UNAVAILABLE, "push failed; retry"),
        }
    }
    let mut head_rx = app.writer.head.clone();
    let wait = Duration::from_secs(req.wait_s.min(25) as u64);
    if acks.is_empty() && *head_rx.borrow() <= req.cursor && !wait.is_zero() {
        let _ = tokio::time::timeout(wait, async {
            while *head_rx.borrow_and_update() <= req.cursor {
                if head_rx.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;
    }
    let (from, hlc) = (req.cursor, *app.writer.hlc.borrow());
    let head = *app.writer.head.borrow();
    let app2 = app.clone();
    let changes = tokio::task::spawn_blocking(move || -> Vec<proto::Changes> {
        let Ok(c) = app2.reader() else { return vec![] };
        let mut out = Vec::new();
        let mut cur = from;
        let mut bytes = 0u64;
        while cur < head && bytes < 4 * CHANGES_PAGE_BYTES {
            match read_changes(&c, cur, head, replica, CHANGES_PAGE_BYTES, hlc) {
                Ok(ch) => {
                    cur = ch.to;
                    bytes += ch
                        .docs
                        .iter()
                        .map(|d| d.bytes.as_ref().map(|b| b.len()).unwrap_or(0) as u64)
                        .sum::<u64>()
                        + 1;
                    out.push(ch);
                }
                Err(_) => break,
            }
        }
        out
    })
    .await
    .unwrap_or_default();
    let resp = HttpSyncResponse {
        welcome: welcome(&app),
        acks,
        changes,
    };
    (
        [(header::CONTENT_TYPE, "application/cbor")],
        proto::encode(&resp),
    )
        .into_response()
}

// ---------------------------------------------------------------- blobs

fn parse_hash(s: &str) -> Option<Hash> {
    Hash::parse_hex(s)
}

#[derive(Deserialize)]
struct BeginReq {
    size: u64,
}

async fn upload_begin(
    State(app): State<Shared>,
    headers: HeaderMap,
    AxPath(hash): AxPath<String>,
    Json(req): Json<BeginReq>,
) -> Response {
    let Some(device) = app.device(&headers) else {
        return unauthorized();
    };
    let Some(h) = parse_hash(&hash) else {
        return err(StatusCode::BAD_REQUEST, "bad hash");
    };
    let now = now_ms();
    let r = app
        .writer
        .call(move |e| {
            e.upload_begin(h, req.size, Some(device), now)
                .map_err(|e| e.to_string())
        })
        .await;
    match r {
        Ok(BeginUpload::Present) => (StatusCode::OK, Json(json!({ "present": true }))).into_response(),
        Ok(BeginUpload::TooLarge) => err(StatusCode::PAYLOAD_TOO_LARGE, "file exceeds JESS_MAX_UPLOAD_MB"),
        Ok(BeginUpload::Session { upload_id, chunk_size, received }) => {
            (StatusCode::CREATED, Json(json!({ "present": false, "upload_id": upload_id, "chunk_size": chunk_size, "received": hex::encode(received) }))).into_response()
        }
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "upload failed"),
    }
}

async fn upload_chunk(
    State(app): State<Shared>,
    headers: HeaderMap,
    AxPath((hash, id, index)): AxPath<(String, String, u32)>,
    body: axum::body::Bytes,
) -> Response {
    if app.device(&headers).is_none() {
        return unauthorized();
    }
    let Some(h) = parse_hash(&hash) else {
        return err(StatusCode::BAD_REQUEST, "bad hash");
    };
    let id2 = id.clone();
    let row = app
        .writer
        .call(move |e| e.upload_get(&id2).ok().flatten())
        .await;
    let Some(row) = row else {
        return err(StatusCode::NOT_FOUND, "unknown upload");
    };
    if row.hash != h {
        return err(StatusCode::BAD_REQUEST, "hash mismatch");
    }
    let offset = index as u64 * row.chunk_size;
    let expected_len = row.size.saturating_sub(offset).min(row.chunk_size);
    if offset >= row.size.max(1) || body.len() as u64 != expected_len {
        return err(StatusCode::BAD_REQUEST, "bad chunk length");
    }
    let given = headers
        .get("x-chunk-sha256")
        .and_then(|v| v.to_str().ok())
        .and_then(Hash::parse_hex);
    if given != Some(Hash::of(&body)) {
        return err(StatusCode::UNPROCESSABLE_ENTITY, "chunk hash mismatch");
    }
    let fs = app.fs.clone();
    let id3 = id.clone();
    let w = tokio::task::spawn_blocking(move || fs.write_chunk(&id3, offset, &body)).await;
    if !matches!(w, Ok(Ok(()))) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "write failed");
    }
    let now = now_ms();
    let ok = app
        .writer
        .call(move |e| e.upload_chunk_done(&id, index, now).unwrap_or(false))
        .await;
    if ok {
        Json(json!({ "ok": true })).into_response()
    } else {
        err(StatusCode::NOT_FOUND, "unknown upload")
    }
}

async fn upload_complete(
    State(app): State<Shared>,
    headers: HeaderMap,
    AxPath((hash, id)): AxPath<(String, String)>,
) -> Response {
    if app.device(&headers).is_none() {
        return unauthorized();
    }
    let Some(h) = parse_hash(&hash) else {
        return err(StatusCode::BAD_REQUEST, "bad hash");
    };
    let id2 = id.clone();
    let row = app
        .writer
        .call(move |e| e.upload_get(&id2).ok().flatten())
        .await;
    let Some(row) = row else {
        let present = app
            .writer
            .call(move |e| e.blob_present(&h).unwrap_or(false))
            .await;
        return if present {
            Json(json!({ "present": true })).into_response()
        } else {
            err(StatusCode::NOT_FOUND, "unknown upload")
        };
    };
    let fs = app.fs.clone();
    let (id3, size) = (id.clone(), row.size);
    let ok = tokio::task::spawn_blocking(move || fs.finalize(&id3, &h, size)).await;
    match ok {
        Ok(Ok(true)) => {
            let now = now_ms();
            let r = app
                .writer
                .call(move |e| e.blob_stored(h, size, Some(&id), now).is_ok())
                .await;
            if r {
                Json(json!({ "present": true })).into_response()
            } else {
                err(StatusCode::INTERNAL_SERVER_ERROR, "could not record blob")
            }
        }
        Ok(Ok(false)) => {
            app.writer.call(move |e| e.upload_discard(&id).ok()).await;
            err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "content hash mismatch; re-hash the source and retry",
            )
        }
        _ => err(StatusCode::INTERNAL_SERVER_ERROR, "finalize failed"),
    }
}

#[derive(Deserialize)]
struct PresenceReq {
    hashes: Vec<String>,
}

async fn blob_presence(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(req): Json<PresenceReq>,
) -> Response {
    if app.device(&headers).is_none() {
        return unauthorized();
    }
    let hs: Vec<Hash> = req
        .hashes
        .iter()
        .filter_map(|h| parse_hash(h))
        .take(10_000)
        .collect();
    let present = app
        .writer
        .call(move |e| e.presence(&hs).unwrap_or_default())
        .await;
    Json(json!({ "present": present.iter().map(|h| h.to_hex()).collect::<Vec<_>>() }))
        .into_response()
}

/// Parses a single `bytes=a-b` / `bytes=a-` / `bytes=-n` range.
pub fn parse_range(v: &str, size: u64) -> Option<(u64, u64)> {
    let r = v.strip_prefix("bytes=")?;
    if r.contains(',') {
        return None;
    }
    let (a, b) = r.split_once('-')?;
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        (size.saturating_sub(n), size.checked_sub(1)?)
    } else {
        let s: u64 = a.parse().ok()?;
        let e: u64 = if b.is_empty() {
            size.checked_sub(1)?
        } else {
            b.parse::<u64>().ok()?.min(size.checked_sub(1)?)
        };
        (s, e)
    };
    if start > end || start >= size {
        return None;
    }
    Some((start, end))
}

pub async fn serve_file(
    path: std::path::PathBuf,
    headers: &HeaderMap,
    method: &Method,
    etag: &str,
    mime: &str,
) -> Response {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let Ok(mut f) = tokio::fs::File::open(&path).await else {
        return err(StatusCode::NOT_FOUND, "not found");
    };
    let size = f.metadata().await.map(|m| m.len()).unwrap_or(0);
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag)
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let mut b = Response::builder()
        .header(
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable",
        )
        .header(header::ETAG, etag)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, mime);
    if mime == "image/svg+xml" {
        b = b.header("content-security-policy", "sandbox");
    }
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|v| parse_range(v, size));
    let (status, start, len) = match range {
        Some(Some((s, e))) => {
            b = b.header(header::CONTENT_RANGE, format!("bytes {s}-{e}/{size}"));
            (StatusCode::PARTIAL_CONTENT, s, e - s + 1)
        }
        Some(None) => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                .body(Body::empty())
                .expect("response");
        }
        None => (StatusCode::OK, 0, size),
    };
    b = b.status(status).header(header::CONTENT_LENGTH, len);
    if method == Method::HEAD {
        return b.body(Body::empty()).expect("response");
    }
    if f.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "seek failed");
    }
    let stream = tokio_util::io::ReaderStream::new(f.take(len));
    b.body(Body::from_stream(stream)).expect("response")
}

async fn blob_get(
    State(app): State<Shared>,
    method: Method,
    headers: HeaderMap,
    AxPath(hash): AxPath<String>,
) -> Response {
    if app.device(&headers).is_none() {
        return unauthorized();
    }
    let Some(h) = parse_hash(&hash) else {
        return err(StatusCode::BAD_REQUEST, "bad hash");
    };
    let mime = app
        .writer
        .call(move |e| {
            e.conn
                .query_row(
                    "SELECT mime FROM blobs WHERE hash = ?1 AND present = 1",
                    [h.0.to_vec()],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
        })
        .await;
    let Some(mime) = mime else {
        return err(StatusCode::NOT_FOUND, "blob not present");
    };
    let mime = mime.unwrap_or_else(|| "application/octet-stream".into());
    serve_file(
        app.fs.path(&h),
        &headers,
        &method,
        &format!("\"{}\"", h.to_hex()),
        &mime,
    )
    .await
}

async fn blob_derived(
    State(app): State<Shared>,
    method: Method,
    headers: HeaderMap,
    AxPath((hash, kind)): AxPath<(String, String)>,
) -> Response {
    if app.device(&headers).is_none() {
        return unauthorized();
    }
    let Some(h) = parse_hash(&hash) else {
        return err(StatusCode::BAD_REQUEST, "bad hash");
    };
    let (dir, mime) = match kind.as_str() {
        "display" => ("display", "image/jpeg"),
        "thumb" => ("thumb", "image/jpeg"),
        "pdf-thumb" => ("pdf-thumb", "image/jpeg"),
        "pdf-text" => ("pdf-text", "application/zstd"),
        _ => return err(StatusCode::NOT_FOUND, "unknown derived kind"),
    };
    let p = app.cfg.data_dir.join("derived").join(dir).join(h.to_hex());
    let mime =
        if dir != "pdf-text" && std::path::Path::new(&format!("{}.png", p.display())).exists() {
            "image/png"
        } else {
            mime
        };
    let p = if mime == "image/png" {
        std::path::PathBuf::from(format!("{}.png", p.display()))
    } else {
        p
    };
    serve_file(
        p,
        &headers,
        &method,
        &format!("\"{}-{kind}\"", h.to_hex()),
        mime,
    )
    .await
}

// ---------------------------------------------------------------- static UI

async fn static_files(State(app): State<Shared>, method: Method, uri: axum::http::Uri) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return err(StatusCode::NOT_FOUND, "not found");
    }
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return err(StatusCode::NOT_FOUND, "not found");
    }
    let safe = !path.split('/').any(|s| s == ".." || s.starts_with('.'));
    let root = &app.cfg.ui_dir;
    let candidate = if path.is_empty() || !safe {
        root.join("index.html")
    } else {
        root.join(path)
    };
    let file = if candidate.is_file() {
        candidate
    } else {
        root.join("index.html")
    };
    let is_index = file.file_name().map(|n| n == "index.html").unwrap_or(false);
    let Ok(bytes) = tokio::fs::read(&file).await else {
        return (StatusCode::OK, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], "<!doctype html><title>Jess</title><p>Jess server is running. The web UI is not installed (set JESS_UI_DIR).").into_response();
    };
    let mime = mime_for(&file);
    let cache = if is_index || path == "sw.js" || path == "manifest.webmanifest" {
        "no-cache"
    } else if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=3600"
    };
    (
        [(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, cache)],
        bytes,
    )
        .into_response()
}

pub fn mime_for(p: &std::path::Path) -> &'static str {
    match p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json") | Some("webmanifest") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::parse_range;
    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-9", 100), Some((0, 9)));
        assert_eq!(parse_range("bytes=90-", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=95-200", 100), Some((95, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=0-1,4-5", 100), None);
    }
}
