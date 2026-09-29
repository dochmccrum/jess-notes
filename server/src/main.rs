//! `jess`: serve | integrity-check [--hashes] [--mirror] | rebuild-mirror | snapshot |
//! reset-password | gc --dry-run

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
        "version" | "--version" => {
            println!("jess {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprintln!("usage: jess [serve | integrity-check [--hashes] [--mirror] | rebuild-mirror | snapshot | reset-password | gc --dry-run | health]");
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
        jess_server::serve::run(app, listener, shutdown_signal(), true).await
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
