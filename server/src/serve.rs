//! Running the server: building the shared state and serving HTTP until shutdown. Used by
//! `jess serve` and by in-process tests (the native client's integration tests).
use crate::auth;
use crate::blobfs::BlobFs;
use crate::config::{now_ms, Config};
use crate::db;
use crate::engine::Engine;
use crate::http::{self, App, Shared};
use crate::writer::Writer;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};

pub fn open_engine(cfg: &Config) -> Result<Engine, String> {
    std::fs::create_dir_all(&cfg.data_dir).map_err(|e| e.to_string())?;
    let conn = db::open(&cfg.db_path(), true).map_err(|e| e.to_string())?;
    Engine::open(conn, cfg.engine_config(), rand::random()).map_err(|e| e.to_string())
}

/// Opens the database (creating the account from `JESS_ADMIN_PASSWORD`, or printing a setup
/// code) and builds the shared app state. Call outside an async context (blocking writer calls).
pub fn build(cfg: &Config) -> Result<Shared, String> {
    std::fs::create_dir_all(&cfg.data_dir)
        .map_err(|e| format!("cannot create {}: {e}", cfg.data_dir.display()))?;
    let fs = BlobFs::new(cfg.blobs_dir(), true).map_err(|e| e.to_string())?;
    let cfg2 = cfg.clone();
    let writer = Writer::spawn(move || {
        open_engine(&cfg2).unwrap_or_else(|e| panic!("cannot open database: {e}"))
    });
    let (vault_id, account, tokens) = writer.call_blocking(|e| {
        (
            e.vault_id,
            auth::account_hash(&e.conn).ok().flatten(),
            auth::load_tokens(&e.conn).unwrap_or_default(),
        )
    });
    let mut setup_code = None;
    if account.is_none() {
        match &cfg.admin_password {
            Some(pw) => {
                let phc = if cfg.random_secret {
                    auth::hash_random_secret(pw)
                } else {
                    auth::hash_password(pw)
                };
                writer
                    .call_blocking(move |e| auth::set_password(&e.conn, &phc, true, now_ms()))
                    .map_err(|e| e.to_string())?;
                tracing::info!("account created from JESS_ADMIN_PASSWORD");
            }
            None => {
                let code = auth::setup_code();
                tracing::warn!(
                    "SETUP CODE: {code}  (enter it on the setup screen to create the account)"
                );
                setup_code = Some(code);
            }
        }
    }
    let app = Arc::new(App {
        cfg: cfg.clone(),
        writer,
        fs,
        vault_id,
        tokens: RwLock::new(tokens),
        revoked: tokio::sync::broadcast::channel(16).0,
        limiter: Mutex::new(crate::auth::RateLimiter::new(cfg.login_per_minute as usize)),
        setup_code: Mutex::new(setup_code),
        started: now_ms(),
        status: Mutex::new(Default::default()),
        mirror: Default::default(),
    });
    Ok(app)
}

/// Serves until `shutdown` resolves, then flushes the mirror and checkpoints the WAL.
/// `derive` starts the attachment-derivation worker (it re-executes the current binary as
/// `jess derive`, so only the real `jess` binary may enable it).
pub async fn run(
    app: Shared,
    listener: tokio::net::TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
    derive: bool,
) -> Result<(), String> {
    crate::tasks::spawn(app.clone());
    if derive {
        crate::derive::spawn(app.clone());
    }
    crate::start_mirror(app.clone());
    let router = http::router(app.clone());
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .map_err(|e| e.to_string())?;
    tracing::info!("shutting down: flushing mirror and checkpointing WAL");
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        crate::flush_mirror(app.clone()),
    )
    .await;
    app.writer
        .call(|e| {
            e.conn
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                .map_err(|e| e.to_string())
        })
        .await?;
    Ok(())
}
