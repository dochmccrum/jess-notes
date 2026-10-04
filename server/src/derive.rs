//! Derived data (DESIGN §8): image display/thumb variants and PDF first-page thumbnail + per-page
//! text, computed once per blob hash.
//!
//! The work runs in a **subprocess** of the same binary (`jess derive image|pdf <in> <outdir>`)
//! with CPU/memory rlimits, a wall-clock timeout and low priority, so a hostile or huge file can
//! only fail its own job. The parent (a tokio task) picks work, moves the outputs into
//! `/data/derived/<kind>/<hex>[.png]` and records `derived` rows. Clients fetch derived files
//! lazily and fall back to the original while a variant doesn't exist yet.

use crate::config::now_ms;
use crate::http::Shared;
use jess_core::Hash;
use rusqlite::params;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Bump when the output of a kind changes, so everything is re-derived.
pub const IMAGE_VERSION: i64 = 1;
pub const PDF_VERSION: i64 = 1;

pub const DISPLAY_EDGE: u32 = 1600;
pub const THUMB_EDGE: u32 = 256;
pub const PDF_THUMB_WIDTH: u32 = 600;
/// Upper bound on extracted PDF text kept per blob (after that, pages are truncated).
const PDF_TEXT_MAX: usize = 32 << 20;

const IMAGE_MIMES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/webp",
    "image/bmp",
    "image/tiff",
];

pub fn is_image_mime(m: &str) -> bool {
    IMAGE_MIMES.contains(&m)
}

// ------------------------------------------------------------------ subprocess side

/// Decodes an image (EXIF-oriented) and writes `display` and `thumb` variants plus `info.json`.
/// Opaque images become JPEG, images with alpha PNG.
pub fn derive_image(input: &Path, out: &Path) -> Result<(), String> {
    use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
    let mut reader = ImageReader::open(input)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(30_000);
    limits.max_image_height = Some(30_000);
    limits.max_alloc = Some(1 << 30);
    reader.limits(limits);
    let mut dec = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = dec
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    let alpha = img.color().has_alpha();
    let (w, h) = (img.width(), img.height());
    let display = if w.max(h) > DISPLAY_EDGE {
        img.resize(
            DISPLAY_EDGE,
            DISPLAY_EDGE,
            image::imageops::FilterType::CatmullRom,
        )
    } else {
        img.clone()
    };
    let thumb = display.thumbnail(THUMB_EDGE, THUMB_EDGE);
    write_variant(&display, alpha, 82, &out.join("display"))?;
    write_variant(&thumb, alpha, 75, &out.join("thumb"))?;
    std::fs::write(
        out.join("info.json"),
        json!({ "width": w, "height": h, "alpha": alpha }).to_string(),
    )
    .map_err(|e| e.to_string())
}

fn write_variant(
    img: &image::DynamicImage,
    alpha: bool,
    quality: u8,
    base: &Path,
) -> Result<(), String> {
    use image::codecs::{jpeg::JpegEncoder, png::PngEncoder};
    use image::ImageEncoder;
    if alpha {
        let rgba = img.to_rgba8();
        let f = std::fs::File::create(base.with_extension("png")).map_err(|e| e.to_string())?;
        PngEncoder::new(std::io::BufWriter::new(f))
            .write_image(
                rgba.as_raw(),
                rgba.width(),
                rgba.height(),
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| e.to_string())
    } else {
        let rgb = img.to_rgb8();
        let f = std::fs::File::create(base.with_extension("jpg")).map_err(|e| e.to_string())?;
        JpegEncoder::new_with_quality(std::io::BufWriter::new(f), quality)
            .write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )
            .map_err(|e| e.to_string())
    }
}

/// Where libpdfium is looked for: `JESS_PDFIUM_LIB` (a file), then the image's `/usr/local/lib`,
/// then the system library path.
pub fn pdfium_library() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("JESS_PDFIUM_LIB") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    ["/usr/local/lib/libpdfium.so", "/usr/lib/libpdfium.so"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// Renders page 1 to `pdf-thumb.jpg` and extracts every page's text to `pdf-text` (zstd JSON).
pub fn derive_pdf(input: &Path, out: &Path) -> Result<(), String> {
    use pdfium_render::prelude::*;
    let lib = pdfium_library().ok_or("pdfium library not found")?;
    let bindings = Pdfium::bind_to_library(lib.to_string_lossy().as_ref())
        .map_err(|e| format!("pdfium: {e}"))?;
    let pdfium = Pdfium::new(bindings);
    let doc = pdfium
        .load_pdf_from_file(input, None)
        .map_err(|e| format!("{e}"))?;
    let pages = doc.pages();
    let n = pages.len();
    let mut first = (0.0f32, 0.0f32);
    if n > 0 {
        let page = pages.get(0).map_err(|e| format!("{e}"))?;
        first = (page.width().value, page.height().value);
        let cfg = PdfRenderConfig::new()
            .set_target_width(PDF_THUMB_WIDTH as i32)
            .set_maximum_height((PDF_THUMB_WIDTH * 3) as i32);
        let img = page
            .render_with_config(&cfg)
            .map_err(|e| format!("{e}"))?
            .as_image()
            .map_err(|e| format!("{e}"))?;
        write_variant(&img, false, 80, &out.join("pdf-thumb"))?;
    }
    let mut texts: Vec<String> = Vec::with_capacity(n as usize);
    let mut total = 0usize;
    for page in pages.iter() {
        let t = if total < PDF_TEXT_MAX {
            page.text().map(|t| t.all()).unwrap_or_default()
        } else {
            String::new()
        };
        total += t.len();
        texts.push(t);
    }
    let body = json!({ "pages": texts }).to_string();
    let z = zstd::encode_all(body.as_bytes(), 9).map_err(|e| e.to_string())?;
    std::fs::write(out.join("pdf-text"), z).map_err(|e| e.to_string())?;
    std::fs::write(
        out.join("info.json"),
        json!({ "pages": n, "width": first.0, "height": first.1 }).to_string(),
    )
    .map_err(|e| e.to_string())
}

/// `jess derive <image|pdf> <input> <outdir>` (the subprocess entry point).
pub fn cli(args: &[String]) -> Result<(), String> {
    let [kind, input, out] = args else {
        return Err("usage: jess derive <image|pdf> <input> <outdir>".into());
    };
    let (input, out) = (Path::new(input), Path::new(out));
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    match kind.as_str() {
        "image" => derive_image(input, out),
        "pdf" => derive_pdf(input, out),
        _ => Err(format!("unknown kind {kind}")),
    }
}

// ------------------------------------------------------------------ parent side

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    Image,
    Pdf,
}

impl Job {
    fn name(self) -> &'static str {
        match self {
            Job::Image => "image",
            Job::Pdf => "pdf",
        }
    }
    fn kinds(self) -> &'static [&'static str] {
        match self {
            Job::Image => &["display", "thumb"],
            Job::Pdf => &["pdf-thumb", "pdf-text"],
        }
    }
    fn version(self) -> i64 {
        match self {
            Job::Image => IMAGE_VERSION,
            Job::Pdf => PDF_VERSION,
        }
    }
}

/// Present blobs that still need derivation, oldest first.
pub fn pending(conn: &rusqlite::Connection, limit: usize) -> rusqlite::Result<Vec<(Hash, Job)>> {
    let mut out = Vec::new();
    let mut q = conn.prepare(
        "SELECT b.hash, b.mime FROM blobs b WHERE b.present = 1 AND b.mime IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM derived d WHERE d.hash = b.hash
                           AND d.kind = (CASE WHEN b.mime = 'application/pdf' THEN 'pdf-thumb' ELSE 'thumb' END)
                           AND d.version >= (CASE WHEN b.mime = 'application/pdf' THEN ?1 ELSE ?2 END))
         ORDER BY b.seq LIMIT ?3",
    )?;
    let rows = q.query_map(
        params![PDF_VERSION, IMAGE_VERSION, (limit * 4) as i64],
        |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?)),
    )?;
    for r in rows {
        let (h, mime) = r?;
        let Ok(h) = <[u8; 32]>::try_from(h.as_slice()) else {
            continue;
        };
        let job = if mime == "application/pdf" {
            Job::Pdf
        } else if is_image_mime(&mime) {
            Job::Image
        } else {
            continue;
        };
        out.push((Hash(h), job));
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// The final path of a derived file (`.png` for PNG outputs).
pub fn derived_path(data: &Path, kind: &str, h: &Hash, png: bool) -> PathBuf {
    let p = data.join("derived").join(kind).join(h.to_hex());
    if png {
        p.with_extension("png")
    } else {
        p
    }
}

/// Runs one job in a subprocess and installs its outputs. Returns the rows to record:
/// `(kind, status, size, error)`.
pub async fn run_job(
    exe: &Path,
    data: &Path,
    blob: &Path,
    h: &Hash,
    job: Job,
    timeout: Duration,
) -> Vec<(&'static str, &'static str, i64, Option<String>)> {
    let tmp =
        data.join("derived")
            .join(".tmp")
            .join(format!("{}-{}", h.to_hex(), rand::random::<u32>()));
    let result = run_subprocess(exe, job, blob, &tmp, timeout).await;
    let rows = match result {
        Ok(()) => install(data, h, job, &tmp),
        Err(e) => {
            let status = if e.contains("pdfium library not found") {
                "unavailable"
            } else {
                "error"
            };
            job.kinds()
                .iter()
                .map(|k| (*k, status, 0, Some(e.clone())))
                .collect()
        }
    };
    let _ = std::fs::remove_dir_all(&tmp);
    rows
}

async fn run_subprocess(
    exe: &Path,
    job: Job,
    blob: &Path,
    tmp: &Path,
    timeout: Duration,
) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("derive")
        .arg(job.name())
        .arg(blob)
        .arg(tmp)
        .env("RUST_LOG", "error")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let cpu = timeout.as_secs().max(5);
    // SAFETY: only async-signal-safe libc calls between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            let lim = |r, v: u64| {
                let l = libc::rlimit {
                    rlim_cur: v as libc::rlim_t,
                    rlim_max: v as libc::rlim_t,
                };
                libc::setrlimit(r, &l);
            };
            lim(libc::RLIMIT_AS, 2 << 30);
            lim(libc::RLIMIT_CPU, cpu);
            lim(libc::RLIMIT_FSIZE, 256 << 20);
            lim(libc::RLIMIT_CORE, 0);
            libc::setpriority(libc::PRIO_PROCESS, 0, 10);
            Ok(())
        });
    }
    let child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Err(_) => Err(format!("timed out after {} s", timeout.as_secs())),
        Ok(Err(e)) => Err(e.to_string()),
        Ok(Ok(o)) if o.status.success() => Ok(()),
        Ok(Ok(o)) => {
            let msg = String::from_utf8_lossy(&o.stderr);
            let msg = msg.trim().trim_start_matches("error: ");
            Err(if msg.is_empty() {
                format!("exited with {}", o.status)
            } else {
                msg.chars().take(500).collect()
            })
        }
    }
}

fn install(
    data: &Path,
    h: &Hash,
    job: Job,
    tmp: &Path,
) -> Vec<(&'static str, &'static str, i64, Option<String>)> {
    let mut rows = Vec::new();
    for kind in job.kinds() {
        let candidates = [
            (tmp.join(format!("{kind}.jpg")), false),
            (tmp.join(format!("{kind}.png")), true),
            (tmp.join(kind), false),
        ];
        let Some((src, png)) = candidates.into_iter().find(|(p, _)| p.is_file()) else {
            rows.push((*kind, "error", 0, Some("no output".to_string())));
            continue;
        };
        let dst = derived_path(data, kind, h, png);
        let other = derived_path(data, kind, h, !png);
        // No fsync per file: the caller syncs once per batch (`sync_derived`) before it records
        // the rows (four fsyncs per image had made derivation ~300 ms a job instead of ~5 ms).
        let r = (|| -> std::io::Result<i64> {
            std::fs::create_dir_all(dst.parent().expect("parent"))?;
            let size = std::fs::metadata(&src)?.len() as i64;
            std::fs::rename(&src, &dst)?;
            let _ = std::fs::remove_file(&other);
            Ok(size)
        })();
        match r {
            Ok(size) => rows.push((*kind, "ok", size, None)),
            Err(e) => rows.push((*kind, "error", 0, Some(e.to_string()))),
        }
    }
    rows
}

/// Makes every derived file written so far durable (`syncfs`), before their rows are recorded.
pub fn sync_derived(data: &Path) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let dir = data.join("derived");
    std::fs::create_dir_all(&dir)?;
    let d = std::fs::File::open(&dir)?;
    // SAFETY: a valid open descriptor for the duration of the call.
    if unsafe { libc::syncfs(d.as_raw_fd()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

type Rows = Vec<(&'static str, &'static str, i64, Option<String>)>;

fn record(conn: &rusqlite::Connection, done: &[(Hash, Job, Rows)]) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    for (h, job, rows) in done {
        for (kind, status, size, error) in rows {
            tx.execute(
                "INSERT OR REPLACE INTO derived(hash, kind, status, version, size, error) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![h.0.to_vec(), kind, status, job.version(), size, error],
            )?;
        }
    }
    tx.commit()
}

/// `jess derive-all`: derives everything pending now, several jobs at a time, offline (the server
/// must be stopped). For after a large `jess import`; the server's own loop does one job at a time
/// in the background. Returns (done, failed).
pub fn derive_all(cfg: &crate::config::Config) -> Result<(u64, u64), String> {
    if pdfium_library().is_none() {
        tracing::warn!("pdfium not found: PDFs get no thumbnails or text");
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let fs = crate::blobfs::BlobFs::new(cfg.blobs_dir(), true).map_err(|e| e.to_string())?;
    let conn = crate::db::open(&cfg.db_path(), true).map_err(|e| e.to_string())?;
    let parallel = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(1, 8);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let (mut done, mut failed) = (0u64, 0u64);
    let mut skipped = std::collections::HashSet::new();
    loop {
        let batch: Vec<(Hash, Job)> = pending(&conn, 64)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|(h, _)| !skipped.contains(h))
            .collect();
        if batch.is_empty() {
            return Ok((done, failed));
        }
        let results = rt.block_on(async {
            use futures_util::stream::{self, StreamExt};
            stream::iter(batch)
                .map(|(h, job)| {
                    let (exe, data, blob) = (exe.clone(), cfg.data_dir.clone(), fs.path(&h));
                    async move {
                        if !blob.is_file() {
                            return (h, job, None);
                        }
                        let rows =
                            run_job(&exe, &data, &blob, &h, job, Duration::from_secs(120)).await;
                        (h, job, Some(rows))
                    }
                })
                .buffer_unordered(parallel)
                .collect::<Vec<_>>()
                .await
        });
        let mut ready = Vec::new();
        for (h, job, rows) in results {
            let Some(rows) = rows else {
                skipped.insert(h);
                continue;
            };
            if rows.iter().any(|r| r.1 != "ok") {
                failed += 1;
            } else {
                done += 1;
            }
            ready.push((h, job, rows));
        }
        sync_derived(&cfg.data_dir).map_err(|e| e.to_string())?;
        record(&conn, &ready).map_err(|e| e.to_string())?;
        eprintln!("{done} derived, {failed} failed");
    }
}

/// Background loop: derive whatever is pending, one job at a time, then idle-poll.
pub fn spawn(app: Shared) {
    tokio::spawn(async move {
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("derivation disabled: {e}");
                return;
            }
        };
        let data = app.cfg.data_dir.clone();
        // A pdfium installed since the last run makes "unavailable" PDFs eligible again.
        if pdfium_library().is_some() {
            let _ = app
                .writer
                .call(|e| {
                    e.conn
                        .execute("DELETE FROM derived WHERE status = 'unavailable'", [])
                })
                .await;
        }
        let mut done = 0u64;
        let mut failed = 0u64;
        loop {
            let batch = tokio::task::spawn_blocking({
                let app = app.clone();
                move || app.reader().and_then(|c| pending(&c, 16))
            })
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default();
            if batch.is_empty() {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            // A batch at a time: derive, one filesystem sync, one transaction for the rows.
            let mut ready = Vec::new();
            for (h, job) in batch {
                let blob = app.fs.path(&h);
                if !blob.is_file() {
                    continue;
                }
                let rows = run_job(&exe, &data, &blob, &h, job, Duration::from_secs(120)).await;
                if rows.iter().any(|r| r.1 != "ok") {
                    failed += 1;
                    tracing::debug!("derive {} {}: {:?}", job.name(), h.to_hex(), rows);
                } else {
                    done += 1;
                }
                ready.push((h, job, rows));
            }
            let d = data.clone();
            let synced = tokio::task::spawn_blocking(move || sync_derived(&d)).await;
            if !matches!(synced, Ok(Ok(()))) {
                tracing::warn!("syncing derived files failed: {synced:?}");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            let r = app.writer.call(move |e| record(&e.conn, &ready)).await;
            if let Err(e) = r {
                tracing::warn!("recording derived rows failed: {e}");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            app.set_status(
                "derive",
                json!({ "ok": true, "done": done, "failed": failed, "pdfium": pdfium_library().is_some(), "at": now_ms() }),
            );
        }
    });
}

/// Reads `pdf-text` back (for tests and tools).
pub fn read_pdf_text(bytes: &[u8]) -> Option<Vec<String>> {
    let raw = zstd::decode_all(bytes).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    Some(
        v["pages"]
            .as_array()?
            .iter()
            .map(|p| p.as_str().unwrap_or("").to_string())
            .collect(),
    )
}
