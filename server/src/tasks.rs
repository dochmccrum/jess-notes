//! Background tasks (DESIGN §2): compaction, trash retention, blob GC, op-result pruning,
//! snapshots. Each is isolated: a failure is logged and surfaced in admin status, never fatal.

use crate::config::now_ms;
use crate::http::Shared;
use serde_json::json;
use std::time::Duration;

pub fn spawn(app: Shared) {
    let a = app.clone();
    tokio::spawn(async move {
        let mut t = tokio::time::interval(Duration::from_secs(60));
        loop {
            t.tick().await;
            let now = now_ms();
            match a.writer.call(move |e| e.compact(now, 100)).await {
                Ok(n) if n > 0 => tracing::debug!("compacted {n} docs"),
                Ok(_) => {}
                Err(e) => tracing::warn!("compaction failed: {e}"),
            }
        }
    });
    let a = app.clone();
    tokio::spawn(async move {
        let mut t = tokio::time::interval(Duration::from_secs(3600));
        loop {
            t.tick().await;
            maintenance(&a).await;
        }
    });
    let a = app;
    tokio::spawn(async move {
        let hours = a.cfg.snapshot_interval_hours.max(1);
        let mut t = tokio::time::interval(Duration::from_secs(hours * 3600));
        loop {
            t.tick().await;
            let (db, dir, days) = (
                a.cfg.db_path(),
                a.cfg.snapshots_dir(),
                a.cfg.snapshot_retention_days,
            );
            let r = tokio::task::spawn_blocking(move || {
                let now = now_ms();
                let p = crate::snapshot::snapshot(&db, &dir, now)?;
                crate::snapshot::prune(&dir, now, days)?;
                Ok::<_, crate::error::Error>(p)
            })
            .await;
            match r {
                Ok(Ok(p)) => {
                    tracing::info!("snapshot written: {}", p.display());
                    a.set_status("snapshot", json!({ "ok": true, "last": p.file_name().map(|n| n.to_string_lossy().to_string()), "at": now_ms() }));
                }
                Ok(Err(e)) => {
                    tracing::error!("snapshot failed: {e}");
                    a.set_status(
                        "snapshot",
                        json!({ "ok": false, "error": e.to_string(), "at": now_ms() }),
                    );
                }
                Err(e) => tracing::error!("snapshot task panicked: {e}"),
            }
        }
    });
}

pub async fn maintenance(a: &Shared) {
    let now = now_ms();
    match a.writer.call(move |e| e.purge_expired_trash(now)).await {
        Ok(n) if n > 0 => tracing::info!("purged {n} expired trash entries"),
        Ok(_) => {}
        Err(e) => tracing::warn!("trash retention failed: {e}"),
    }
    let fs = a.fs.clone();
    let r = a
        .writer
        .call(move |e| {
            let rep = e.gc(now, false)?;
            let n = e.gc_sweep_files(&fs, &rep.deleted)?;
            for u in &rep.expired_uploads {
                fs.discard_tmp(u);
            }
            e.prune_op_results(now)?;
            Ok::<_, crate::error::Error>((rep.marked, n))
        })
        .await;
    match r {
        Ok((marked, deleted)) => a.set_status(
            "gc",
            json!({ "ok": true, "marked": marked, "deleted": deleted, "at": now }),
        ),
        Err(e) => {
            tracing::warn!("gc failed: {e}");
            a.set_status(
                "gc",
                json!({ "ok": false, "error": e.to_string(), "at": now }),
            );
        }
    }
}
