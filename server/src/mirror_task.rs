//! The isolated mirror + git background task (DESIGN §13). A failure here is logged, retried
//! with backoff and shown in admin status; it never blocks, slows or corrupts sync.

use crate::blobfs::BlobFs;
use crate::config::{now_ms, Config};
use crate::git::{commit_message, Git, GitConfig};
use crate::http::Shared;
use crate::mirror::{Changes, Mirror};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const DEBOUNCE: Duration = Duration::from_secs(2);
pub const MAX_COMMIT_INTERVAL_MS: u64 = 10 * 60_000;
pub const GC_EVERY_MS: u64 = 7 * 86_400_000;

pub fn git_config(cfg: &Config) -> GitConfig {
    GitConfig {
        repo: cfg.mirror_dir(),
        key_dir: cfg.data_dir.join("git"),
        remote: cfg.git_remote.clone(),
        include_attachments: cfg.git_include_attachments,
        lfs: cfg.git_lfs,
    }
}

pub fn open_mirror(cfg: &Config) -> crate::error::Result<Mirror> {
    let fs = BlobFs::new(cfg.blobs_dir(), true)?;
    Mirror::open(
        cfg.mirror_dir(),
        &cfg.data_dir.join("mirror-state.db"),
        cfg.db_path(),
        fs,
        true,
    )
}

pub fn start(app: Shared) {
    if !app.cfg.mirror_enabled {
        app.set_status("mirror", json!({ "enabled": false }));
        return;
    }
    tokio::spawn(async move {
        let cfg = app.cfg.clone();
        let mirror = match tokio::task::spawn_blocking(move || open_mirror(&cfg)).await {
            Ok(Ok(m)) => Arc::new(Mutex::new(m)),
            Ok(Err(e)) => {
                tracing::error!("mirror disabled: {e}");
                app.set_status("mirror", json!({ "ok": false, "error": e.to_string() }));
                return;
            }
            Err(_) => return,
        };
        let _ = app.mirror.set(mirror.clone());
        let git = if app.cfg.git_enabled && Git::available() {
            let g = Git {
                cfg: git_config(&app.cfg),
            };
            let r = tokio::task::spawn_blocking(move || {
                g.init()?;
                let key = g.deploy_key().ok();
                Ok::<_, crate::git::GitError>((g, key))
            })
            .await;
            match r {
                Ok(Ok((g, key))) => {
                    app.set_status(
                        "git",
                        json!({ "ok": true, "deploy_key": key, "remote": app.cfg.git_remote }),
                    );
                    Some(Arc::new(g))
                }
                Ok(Err(e)) => {
                    tracing::warn!("git disabled: {e}");
                    app.set_status("git", json!({ "ok": false, "error": e.0 }));
                    None
                }
                Err(_) => None,
            }
        } else {
            app.set_status("git", json!({ "enabled": false, "reason": if app.cfg.git_enabled { "git binary not found" } else { "JESS_GIT_ENABLED=false" } }));
            None
        };
        let mut head_rx = app.writer.head.clone();
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        let mut dirty = true;
        let mut backoff = Duration::from_secs(1);
        let mut retry_at: Option<tokio::time::Instant> = None;
        let mut pending = Changes::default();
        let (mut first_pending, mut last_change) = (0u64, 0u64);
        let mut push_due = false;
        let mut push_backoff = Duration::from_secs(30);
        let mut push_at = tokio::time::Instant::now();
        let mut last_gc = now_ms();
        let commit_quiet = app.cfg.git_commit_interval_s * 1000;
        loop {
            tokio::select! {
                r = head_rx.changed() => {
                    if r.is_err() { return; }
                    dirty = true;
                    // Debounce: let a burst of commits settle.
                    tokio::time::sleep(DEBOUNCE).await;
                }
                _ = tick.tick() => {}
            }
            if dirty
                && retry_at
                    .map(|t| tokio::time::Instant::now() >= t)
                    .unwrap_or(true)
            {
                let m = mirror.clone();
                let r =
                    tokio::task::spawn_blocking(move || m.lock().expect("mirror lock").sync_once())
                        .await;
                let head = *app.writer.head.borrow();
                match r {
                    Ok(Ok(ch)) => {
                        dirty = false;
                        retry_at = None;
                        backoff = Duration::from_secs(1);
                        let last = mirror.lock().map(|m| m.last_seq()).unwrap_or(0);
                        let mode = mirror
                            .lock()
                            .ok()
                            .and_then(|m| m.mode)
                            .map(|m| format!("{m:?}"));
                        if !ch.is_empty() {
                            if pending.is_empty() {
                                first_pending = now_ms();
                            }
                            last_change = now_ms();
                            pending.absorb(ch);
                        }
                        app.set_status("mirror", json!({ "ok": true, "last_seq": last, "head_seq": head, "blob_links": mode, "at": now_ms() }));
                    }
                    Ok(Err(e)) => {
                        tracing::warn!("mirror sync failed: {e}; retrying in {backoff:?}");
                        app.set_status(
                            "mirror",
                            json!({ "ok": false, "error": e.to_string(), "at": now_ms() }),
                        );
                        retry_at = Some(tokio::time::Instant::now() + backoff);
                        backoff = (backoff * 2).min(Duration::from_secs(300));
                    }
                    Err(_) => {}
                }
            }
            let Some(g) = git.clone() else { continue };
            let now = now_ms();
            if !pending.is_empty()
                && (now >= last_change + commit_quiet
                    || now >= first_pending + MAX_COMMIT_INTERVAL_MS)
            {
                let msg = commit_message(&pending);
                let g2 = g.clone();
                match tokio::task::spawn_blocking(move || g2.commit(&msg)).await {
                    Ok(Ok(id)) => {
                        pending = Changes::default();
                        push_due = id.is_some() || push_due;
                        app.set_status("git", json!({ "ok": true, "last_commit": id, "at": now, "remote": app.cfg.git_remote, "deploy_key": g.deploy_key().ok() }));
                    }
                    Ok(Err(e)) => {
                        tracing::warn!("git commit failed: {e}");
                        app.set_status("git", json!({ "ok": false, "error": e.0, "at": now }));
                    }
                    Err(_) => {}
                }
            }
            if push_due && app.cfg.git_remote.is_some() && tokio::time::Instant::now() >= push_at {
                let g2 = g.clone();
                match tokio::task::spawn_blocking(move || g2.push()).await {
                    Ok(Ok(())) => {
                        push_due = false;
                        push_backoff = Duration::from_secs(30);
                        app.set_status("git_push", json!({ "ok": true, "at": now }));
                    }
                    Ok(Err(e)) => {
                        tracing::warn!("git push failed (will retry in {push_backoff:?}): {e}");
                        app.set_status("git_push", json!({ "ok": false, "error": e.0, "at": now, "retry_in_s": push_backoff.as_secs() }));
                        push_at = tokio::time::Instant::now() + push_backoff;
                        push_backoff = (push_backoff * 2).min(Duration::from_secs(3600));
                    }
                    Err(_) => {}
                }
            }
            if now >= last_gc + GC_EVERY_MS {
                last_gc = now;
                let g2 = g.clone();
                let _ = tokio::task::spawn_blocking(move || g2.gc_full()).await;
            }
        }
    });
}

/// Graceful shutdown: one last mirror sync (bounded by the caller's timeout).
pub async fn flush(app: Shared) {
    if let Some(m) = app.mirror.get().cloned() {
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(mut m) = m.lock() {
                let _ = m.sync_once();
            }
        })
        .await;
    }
}

pub fn rebuild(cfg: &Config) -> crate::error::Result<()> {
    let mut m = open_mirror(cfg)?;
    let ch = m.rebuild()?;
    println!("mirror rebuilt: {} files", ch.added.len() + ch.edited.len());
    if cfg.git_enabled && Git::available() {
        let g = Git {
            cfg: git_config(cfg),
        };
        g.init().map_err(|e| crate::error::Error::Other(e.0))?;
        if let Some(id) = g
            .commit("Jess: rebuild mirror")
            .map_err(|e| crate::error::Error::Other(e.0))?
        {
            println!("committed {id}");
        }
    }
    Ok(())
}

pub fn check(cfg: &Config) -> crate::error::Result<Vec<String>> {
    let fs = BlobFs::new(cfg.blobs_dir(), true)?;
    crate::mirror::check(&cfg.mirror_dir(), &cfg.db_path(), &fs)
}
