//! `jess`: serve | integrity-check [--hashes] [--mirror] | rebuild-mirror | snapshot |
//! reset-password | gc --dry-run | import <folder|zip> [--dry-run] [--conflict skip|overwrite|keep_both] |
//! derive-all

use jess_server::auth;
use jess_server::blobfs::BlobFs;
use jess_server::config::{now_ms, Config};
use jess_server::db;
use jess_server::engine::Engine;
use std::io::BufRead;
use std::net::SocketAddr;
use std::process::ExitCode;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("serve");
    let flag = |f: &str| args.iter().any(|a| a == f);
    let cfg = Config::from_env();
    let r = match cmd {
        "serve" => serve(cfg),
        "integrity-check" => integrity(cfg, flag("--hashes"), flag("--mirror")),
        "snapshot" => {
            jess_server::snapshot::snapshot(&cfg.db_path(), &cfg.snapshots_dir(), now_ms())
                .map(|p| println!("{}", p.display()))
                .map_err(|e| e.to_string())
        }
        "reset-password" => reset_password(cfg),
        "gc" => gc(cfg, flag("--dry-run")),
        "rebuild-mirror" => rebuild_mirror(cfg),
        "health" => health(&cfg),
        "derive" => jess_server::derive::cli(&args[1..]),
        "derive-all" => derive_all(cfg),
        "import" => import(cfg, &args[1..]),
        "version" | "--version" => {
            println!("jess {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprintln!("usage: jess [serve | integrity-check [--hashes] [--mirror] | rebuild-mirror | snapshot | reset-password | gc --dry-run | health | import <folder|zip> [--dry-run] [--conflict skip|overwrite|keep_both] | derive-all]");
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Container healthcheck: GET /healthz on the local port (no curl in the image).
fn health(cfg: &Config) -> Result<(), String> {
    use std::io::{Read, Write};
    let addr = SocketAddr::from(([127, 0, 0, 1], cfg.port));
    let mut s = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(3))
        .map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    s.write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .map_err(|e| e.to_string())?;
    let mut buf = [0u8; 32];
    let n = s.read(&mut buf).map_err(|e| e.to_string())?;
    let head = String::from_utf8_lossy(&buf[..n]);
    if head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.0 200") {
        Ok(())
    } else {
        Err(format!("unhealthy: {}", head.lines().next().unwrap_or("")))
    }
}

fn open_engine(cfg: &Config) -> Result<Engine, String> {
    jess_server::serve::open_engine(cfg)
}

fn integrity(cfg: Config, hashes: bool, mirror: bool) -> Result<(), String> {
    let conn = db::open_readonly(&cfg.db_path()).map_err(|e| e.to_string())?;
    let fs = BlobFs::new(cfg.blobs_dir(), true).map_err(|e| e.to_string())?;
    let mut problems =
        jess_server::integrity::check(&conn, &fs, &jess_server::integrity::Options { hashes })
            .map_err(|e| e.to_string())?;
    if mirror {
        problems.extend(jess_server::mirror_check(&cfg).map_err(|e| e.to_string())?);
    }
    if problems.is_empty() {
        println!("integrity-check: ok");
        Ok(())
    } else {
        for p in &problems {
            println!("PROBLEM: {p}");
        }
        Err(format!("{} problem(s)", problems.len()))
    }
}

fn reset_password(cfg: Config) -> Result<(), String> {
    let pw = match std::env::var("JESS_NEW_PASSWORD") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("New password (read from stdin):");
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .map_err(|e| e.to_string())?;
            line.trim_end_matches(['\r', '\n']).to_string()
        }
    };
    if pw.chars().count() < 8 {
        return Err("password must be at least 8 characters".into());
    }
    let conn = db::open(&cfg.db_path(), true).map_err(|e| e.to_string())?;
    auth::set_password(&conn, &auth::hash_password(&pw), false, now_ms())
        .map_err(|e| e.to_string())?;
    println!("password updated");
    Ok(())
}

fn gc(cfg: Config, dry_run: bool) -> Result<(), String> {
    let mut e = open_engine(&cfg)?;
    let rep = e.gc(now_ms(), dry_run).map_err(|e| e.to_string())?;
    if !dry_run {
        let fs = BlobFs::new(cfg.blobs_dir(), true).map_err(|e| e.to_string())?;
        e.gc_sweep_files(&fs, &rep.deleted)
            .map_err(|e| e.to_string())?;
    }
    println!(
        "{} blob(s) {}",
        rep.deleted.len(),
        if dry_run {
            "would be deleted"
        } else {
            "deleted"
        }
    );
    for h in rep.deleted {
        println!("  {h}");
    }
    Ok(())
}

/// Imports an Obsidian vault folder or zip into the database with the same planner as the
/// clients and `POST /api/admin/import` (DESIGN §12). Offline: the server must be stopped.
fn import(cfg: Config, args: &[String]) -> Result<(), String> {
    use jess_core::import::{self, Conflict, FolderSource, PlanOptions, ZipSource};
    use jess_server::vault_io::ServerSink;
    let mut path = None;
    let mut dry = false;
    let mut conflict = Conflict::Skip;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dry-run" => dry = true,
            "--conflict" => {
                conflict = match it.next().map(String::as_str) {
                    Some("skip") => Conflict::Skip,
                    Some("overwrite") => Conflict::Overwrite,
                    Some("keep_both") => Conflict::KeepBoth,
                    _ => return Err("--conflict takes skip, overwrite or keep_both".into()),
                }
            }
            p if path.is_none() && !p.starts_with("--") => path = Some(std::path::PathBuf::from(p)),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let path = path.ok_or("usage: jess import <folder|zip> [--dry-run] [--conflict …]")?;
    if health(&cfg).is_ok() {
        return Err(format!(
            "a server is running on port {}: stop it first (or import from the app's settings)",
            cfg.port
        ));
    }
    let mut src: Box<dyn import::ImportSource> = if path.is_dir() {
        Box::new(FolderSource { root: path })
    } else {
        let f = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Box::new(ZipSource::new(std::io::BufReader::new(f)).map_err(|e| e.to_string())?)
    };
    let mut e = open_engine(&cfg)?;
    let fs = BlobFs::new(cfg.blobs_dir(), true).map_err(|e| e.to_string())?;
    struct Ex<'a>(&'a mut Engine);
    impl import::Existing for Ex<'_> {
        fn state(&self) -> &jess_core::state::MetaState {
            &self.0.state
        }
        fn text(&mut self, id: jess_core::Id) -> Option<Vec<u8>> {
            self.0.doc_text(id, "body").ok().map(String::into_bytes)
        }
    }
    let opts = PlanOptions {
        conflict,
        ..Default::default()
    };
    let t0 = std::time::Instant::now();
    let plan = import::plan(&mut *src, &mut Ex(&mut e), &opts).map_err(|e| e.to_string())?;
    let r = &plan.report;
    println!(
        "{} note(s), {} image(s), {} PDF(s), {} other, {} folder(s); {} unchanged, {} conflict(s), {} skipped, {} unresolved link(s)",
        r.notes, r.images, r.pdfs, r.other_media, r.folders, r.unchanged, r.conflicts, r.skipped.len(), r.unresolved.len()
    );
    for w in &r.warnings {
        println!("warning: {w}");
    }
    if dry {
        return Ok(());
    }
    let state = e.state.clone();
    let mut sink = ServerSink::new(&mut e, &fs, now_ms(), rand::random());
    let mut last = 0;
    import::execute(&plan, &mut *src, &state, &mut sink, |done, total| {
        if done == total || done >= last + 1000 {
            last = done;
            eprintln!("{done}/{total}");
        }
    })
    .map_err(|e| e.to_string())?;
    for r in &sink.rejected {
        println!("rejected: {r}");
    }
    println!("imported in {:.1} s", t0.elapsed().as_secs_f64());
    Ok(())
}

/// Derives every pending thumbnail and PDF text now (offline): after a large import.
fn derive_all(cfg: Config) -> Result<(), String> {
    if health(&cfg).is_ok() {
        return Err(format!(
            "a server is running on port {}: stop it first (it derives in the background anyway)",
            cfg.port
        ));
    }
    let t0 = std::time::Instant::now();
    let (done, failed) = jess_server::derive::derive_all(&cfg)?;
    println!(
        "{done} derived, {failed} failed, in {:.1} s",
        t0.elapsed().as_secs_f64()
    );
    Ok(())
}

fn rebuild_mirror(cfg: Config) -> Result<(), String> {
    jess_server::rebuild_mirror(&cfg).map_err(|e| e.to_string())
}

fn serve(cfg: Config) -> Result<(), String> {
    let app = jess_server::serve::build(&cfg)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let addr = SocketAddr::from(([0, 0, 0, 0], cfg.port));
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| format!("bind {addr}: {e}"))?;
        tracing::info!(
            "jess listening on {addr} (data: {})",
            cfg.data_dir.display()
        );
        // JESS_DERIVE=false: no thumbnails or PDF text in the background (the benchmarks, whose
        // server would be on another machine; `jess derive-all` does it offline).
        let derive = !matches!(
            std::env::var("JESS_DERIVE").as_deref(),
            Ok("false" | "0" | "no" | "off")
        );
        jess_server::serve::run(app, listener, shutdown_signal(), derive).await
    })
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
}
