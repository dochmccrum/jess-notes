//! Derived data (DESIGN §8): the `jess derive` subprocess and the server's derivation loop.
//! PDF parts run only when pdfium is available (`JESS_PDFIUM_LIB` or the image's path).

mod common;
use common::*;
use image::{DynamicImage, GenericImageView, ImageBuffer, Rgb, Rgba};
use jess_core::Hash;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn derive(kind: &str, input: &Path, out: &Path) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_jess"))
        .args(["derive", kind])
        .arg(input)
        .arg(out)
        .output()
        .unwrap();
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

fn pdfium() -> bool {
    jess_server::derive::pdfium_library().is_some()
}

/// A JPEG with an EXIF APP1 segment carrying `orientation`.
fn jpeg_with_orientation(w: u32, h: u32, orientation: u16) -> Vec<u8> {
    let img = ImageBuffer::from_fn(w, h, |x, _| Rgb([(x % 256) as u8, 40, 200]));
    let mut jpg = Vec::new();
    DynamicImage::ImageRgb8(img)
        .write_to(
            &mut std::io::Cursor::new(&mut jpg),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
    // TIFF header (big endian) + IFD0 with one entry: Orientation (0x0112), SHORT, 1.
    let mut tiff = b"MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01".to_vec();
    tiff.extend_from_slice(&orientation.to_be_bytes());
    tiff.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff);
    let len = (app1.len() + 2) as u16;
    let mut out = jpg[..2].to_vec(); // SOI
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpg[2..]);
    out
}

#[test]
fn image_variants_are_oriented_and_bounded() {
    let dir = tempfile::tempdir().unwrap();
    // 3000x1000, rotated 90° by EXIF → displayed 1000x3000.
    let src = dir.path().join("photo.jpg");
    std::fs::write(&src, jpeg_with_orientation(3000, 1000, 6)).unwrap();
    let out = dir.path().join("out");
    let (ok, err) = derive("image", &src, &out);
    assert!(ok, "{err}");
    let d = image::open(out.join("display.jpg")).unwrap();
    assert_eq!(d.dimensions(), (533, 1600));
    let t = image::open(out.join("thumb.jpg")).unwrap();
    assert!(t.width().max(t.height()) <= 256);
    assert!(t.height() > t.width());
    let info: serde_json::Value =
        serde_json::from_slice(&std::fs::read(out.join("info.json")).unwrap()).unwrap();
    assert_eq!(
        (info["width"].as_u64(), info["height"].as_u64()),
        (Some(1000), Some(3000))
    );

    // Alpha → PNG, small images are not upscaled.
    let png = dir.path().join("icon.png");
    ImageBuffer::from_fn(100, 50, |x, _| Rgba([255, 0, 0, (x * 2) as u8]))
        .save(&png)
        .unwrap();
    let out2 = dir.path().join("out2");
    let (ok, err) = derive("image", &png, &out2);
    assert!(ok, "{err}");
    let d = image::open(out2.join("display.png")).unwrap();
    assert_eq!(d.dimensions(), (100, 50));
    assert!(d.color().has_alpha());
}

#[test]
fn hostile_inputs_fail_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let junk = dir.path().join("junk.png");
    std::fs::write(&junk, b"\x89PNG\r\n\x1a\nthis is not a png").unwrap();
    let (ok, err) = derive("image", &junk, &dir.path().join("o"));
    assert!(!ok);
    assert!(!err.is_empty());
    // A "PNG" header declaring a 100k x 100k image is refused by the limits, not allocated.
    let mut bomb = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    bomb.extend_from_slice(&100_000u32.to_be_bytes());
    bomb.extend_from_slice(&100_000u32.to_be_bytes());
    bomb.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
    let b = dir.path().join("bomb.png");
    std::fs::write(&b, bomb).unwrap();
    let (ok, _) = derive("image", &b, &dir.path().join("o2"));
    assert!(!ok);
    if pdfium() {
        let p = dir.path().join("bad.pdf");
        std::fs::write(&p, b"%PDF-1.4 garbage").unwrap();
        let (ok, _) = derive("pdf", &p, &dir.path().join("o3"));
        assert!(!ok);
    }
}

#[test]
fn pdf_thumb_and_text() {
    if !pdfium() {
        eprintln!("skipping: pdfium not available (set JESS_PDFIUM_LIB)");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let vault = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/vault");
    let pdf = find(&vault, "manual.pdf").expect("fixture pdf");
    let out = dir.path().join("out");
    let (ok, err) = derive("pdf", &pdf, &out);
    assert!(ok, "{err}");
    let thumb = image::open(out.join("pdf-thumb.jpg")).unwrap();
    assert_eq!(thumb.width(), jess_server::derive::PDF_THUMB_WIDTH);
    let pages =
        jess_server::derive::read_pdf_text(&std::fs::read(out.join("pdf-text")).unwrap()).unwrap();
    assert!(!pages.is_empty());
}

fn find(dir: &Path, name: &str) -> Option<std::path::PathBuf> {
    for e in std::fs::read_dir(dir).ok()? {
        let e = e.ok()?;
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find(&p, name) {
                return Some(f);
            }
        } else if e.file_name() == name {
            return Some(p);
        }
    }
    None
}

#[test]
fn server_derives_imported_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let srv = Server::start(dir.path());
    let token = srv.login("derive");
    let auth = format!("Bearer {token}");
    // Import the fixture vault server-side; its images and PDFs become blobs with mime types.
    let vault = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/vault");
    let zip = {
        let mut z = jess_core::zipstream::ZipStream::new(Vec::new());
        let mut stack = vec![(vault.clone(), String::new())];
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
                    z.add_bytes(&r, &std::fs::read(e.path()).unwrap(), None, true)
                        .unwrap();
                }
            }
        }
        z.finish().unwrap()
    };
    let h = upload(&srv, &auth, &zip);
    ureq::post(&srv.url("/api/admin/import"))
        .header("authorization", &auth)
        .send_json(serde_json::json!({ "zip_hash": h.to_hex() }))
        .unwrap();
    let png = Hash::of(&std::fs::read(find(&vault, "shared.png").unwrap()).unwrap());
    let pdf = Hash::of(&std::fs::read(find(&vault, "manual.pdf").unwrap()).unwrap());
    let get = |h: &Hash, kind: &str| {
        ureq::get(&srv.url(&format!("/api/blobs/{h}/derived/{kind}")))
            .header("authorization", &auth)
            .call()
            .ok()
            .filter(|r| r.status() == 200)
            .map(|mut r| {
                let ct = r
                    .headers()
                    .get("content-type")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string();
                (ct, r.body_mut().read_to_vec().unwrap())
            })
    };
    let t = Instant::now();
    let mut thumb = None;
    while t.elapsed() < Duration::from_secs(60) && thumb.is_none() {
        thumb = get(&png, "thumb");
        std::thread::sleep(Duration::from_millis(200));
    }
    if thumb.is_none() {
        let conn = rusqlite::Connection::open(dir.path().join("jess.db")).unwrap();
        let rows: Vec<String> = conn
            .prepare("SELECT hex(hash) || ' ' || ifnull(mime,'-') || ' ' || present FROM blobs")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let d: Vec<String> = conn
            .prepare("SELECT hex(hash) || ' ' || kind || ' ' || status || ' ' || ifnull(error,'') FROM derived")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        panic!("no thumb for {png}; blobs: {rows:#?}\nderived: {d:#?}");
    }
    let (ct, bytes) = thumb.unwrap();
    assert!(ct.starts_with("image/"), "{ct}");
    image::load_from_memory(&bytes).unwrap();
    assert!(get(&png, "display").is_some());
    // No token → 401, as for originals.
    assert_eq!(
        ureq::get(&srv.url(&format!("/api/blobs/{png}/derived/thumb")))
            .call()
            .map(|r| r.status().as_u16())
            .unwrap_or_else(|e| match e {
                ureq::Error::StatusCode(c) => c,
                _ => 0,
            }),
        401
    );
    if pdfium() {
        let t = Instant::now();
        let mut text = None;
        while t.elapsed() < Duration::from_secs(60) && text.is_none() {
            text = get(&pdf, "pdf-text");
            std::thread::sleep(Duration::from_millis(200));
        }
        let (_, bytes) = text.expect("pdf text derived");
        assert!(jess_server::derive::read_pdf_text(&bytes).is_some());
        assert!(get(&pdf, "pdf-thumb").is_some());
    }
    let status: serde_json::Value = ureq::get(&srv.url("/api/admin/status"))
        .header("authorization", &auth)
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert!(
        status["derive"]["done"].as_u64().unwrap_or(0) > 0,
        "{status}"
    );
    let (ok, out) = integrity(dir.path(), false);
    assert!(ok, "{out}");
}

fn upload(srv: &Server, auth: &str, bytes: &[u8]) -> Hash {
    let h = Hash::of(bytes);
    let v: serde_json::Value = ureq::post(&srv.url(&format!("/api/blobs/{h}/uploads")))
        .header("authorization", auth)
        .send_json(serde_json::json!({ "size": bytes.len() }))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    if v["present"] == true {
        return h;
    }
    let id = v["upload_id"].as_str().unwrap().to_string();
    let cs = v["chunk_size"].as_u64().unwrap() as usize;
    for (i, c) in bytes.chunks(cs).enumerate() {
        ureq::put(&srv.url(&format!("/api/blobs/{h}/uploads/{id}/chunks/{i}")))
            .header("authorization", auth)
            .header("x-chunk-sha256", &Hash::of(c).to_hex())
            .send(c)
            .unwrap();
    }
    ureq::post(&srv.url(&format!("/api/blobs/{h}/uploads/{id}/complete")))
        .header("authorization", auth)
        .send_empty()
        .unwrap();
    h
}
