//! Export (DESIGN §12.2): the projection written as a streamed zip or into a folder.
//! Read-only against every store: content comes through `ContentSource`.

use crate::ids::{Hash, Id};
use crate::projection::{Projection, Source};
use crate::zipstream::ZipStream;
use std::io::{self, Read, Write};

pub trait ContentSource {
    /// Exact bytes of a markdown note's text.
    fn text(&mut self, id: Id) -> io::Result<Vec<u8>>;
    /// A reader over a blob and its size.
    fn blob(&mut self, hash: Hash) -> io::Result<(Box<dyn Read + '_>, u64)>;
}

fn is_text_path(p: &str) -> bool {
    p.len() >= 3 && p[p.len() - 3..].eq_ignore_ascii_case(".md")
}

/// Streams the projection as a zip. Media and PDFs are stored; `.md` files are deflated.
/// Adds `EXPORT-REPORT.txt` when the profile changed names or something couldn't be exported.
pub fn write_zip<W: Write>(
    p: &Projection,
    src: &mut dyn ContentSource,
    w: W,
    mut progress: impl FnMut(usize, usize),
) -> io::Result<W> {
    let mut z = ZipStream::new(w);
    let mut skipped = Vec::new();
    let total = p.items.len();
    for (i, it) in p.items.iter().enumerate() {
        match &it.source {
            Source::Dir => z.add_dir(&it.path, it.modified_at)?,
            Source::Text(id) => {
                let b = src.text(*id)?;
                z.add_bytes(&it.path, &b, it.modified_at, true)?;
            }
            Source::Blob(h) => match src.blob(*h) {
                Ok((mut r, size)) => z.add_file(
                    &it.path,
                    size,
                    it.modified_at,
                    is_text_path(&it.path),
                    &mut r,
                )?,
                // Not on this device and not fetchable (offline, or its uploader hasn't finished):
                // leave it out and say so rather than failing the whole export.
                Err(e) => skipped.push(format!("{} ({e})", it.path)),
            },
        }
        progress(i + 1, total);
    }
    if let Some(report) = report_with(p, skipped) {
        z.add_bytes("EXPORT-REPORT.txt", report.as_bytes(), None, true)?;
    }
    z.finish()
}

/// Writes the projection into `dest` (created if needed). Each file is written to a temp name,
/// fsynced and renamed into place, so an interrupted export never leaves partial files.
pub fn write_folder(
    p: &Projection,
    src: &mut dyn ContentSource,
    dest: &std::path::Path,
) -> io::Result<()> {
    std::fs::create_dir_all(dest)?;
    let mut skipped = Vec::new();
    for it in &p.items {
        let target = dest.join(&it.path);
        match &it.source {
            Source::Dir => std::fs::create_dir_all(&target)?,
            Source::Text(id) => {
                let b = src.text(*id)?;
                atomic_write(&target, &mut &b[..])?;
            }
            Source::Blob(h) => match src.blob(*h) {
                Ok((mut r, _)) => atomic_write(&target, &mut r)?,
                Err(e) => skipped.push(format!("{} ({e})", it.path)),
            },
        }
    }
    if let Some(report) = report_with(p, skipped) {
        atomic_write(&dest.join("EXPORT-REPORT.txt"), &mut report.as_bytes())?;
    }
    Ok(())
}

/// The report, if there is anything to report: profile mappings, projection errors, and blobs
/// that couldn't be read during this export.
fn report_with(p: &Projection, skipped: Vec<String>) -> Option<String> {
    if p.mapped.is_empty() && p.errors.is_empty() && skipped.is_empty() {
        return None;
    }
    if skipped.is_empty() {
        return Some(p.report());
    }
    let mut q = p.clone();
    q.errors.extend(skipped);
    Some(q.report())
}

pub fn atomic_write(target: &std::path::Path, r: &mut dyn Read) -> io::Result<()> {
    let dir = target.parent().unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".jess-tmp-{}",
        std::process::id() as u64 ^ target.as_os_str().len() as u64 ^ now_nanos()
    ));
    let mut f = std::fs::File::create(&tmp)?;
    io::copy(r, &mut f)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, target)?;
    Ok(())
}

fn now_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(0)
}
