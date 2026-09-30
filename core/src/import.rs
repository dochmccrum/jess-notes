//! Import planner and executor (DESIGN §12.3). One planner for every source (folder, zip) and
//! every host (web worker, Tauri, server-side zip import). A dry run is the plan, not executed.
//! Content is never modified: notes are imported byte-for-byte.

use crate::ids::{Hash, Id, VAULT_SETTINGS_ID};
use crate::links::{extract, Syntax};
use crate::model::{BlobInfo, PropValue, KIND_FOLDER, KIND_MARKDOWN, KIND_MEDIA, KIND_PDF};
use crate::ops::MetaOp;
use crate::resolve::ResolveIndex;
use crate::state::MetaState;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, Read};

pub const BATCH: usize = 250;

#[derive(Clone, Debug)]
pub struct SourceFile {
    /// Vault-relative path, `/`-separated, exactly as in the source.
    pub path: String,
    pub size: u64,
    pub mtime: Option<u64>,
    pub ctime: Option<u64>,
    /// Compressed size (zip) for bomb detection.
    pub compressed: Option<u64>,
}

pub trait ImportSource {
    /// Files and directories (directories end without a slash) in the source.
    fn list(&mut self) -> io::Result<(Vec<SourceFile>, Vec<String>)>;
    fn open(&mut self, path: &str) -> io::Result<Box<dyn Read + '_>>;
    fn read_all(&mut self, path: &str) -> io::Result<Vec<u8>> {
        let mut v = Vec::new();
        self.open(path)?.read_to_end(&mut v)?;
        Ok(v)
    }
}

// ------------------------------------------------------------------------ sources

/// A folder on disk (native).
pub struct FolderSource {
    pub root: std::path::PathBuf,
}

fn systime_ms(t: io::Result<std::time::SystemTime>) -> Option<u64> {
    t.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
}

impl ImportSource for FolderSource {
    fn list(&mut self) -> io::Result<(Vec<SourceFile>, Vec<String>)> {
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        let mut stack = vec![(self.root.clone(), String::new())];
        while let Some((dir, rel)) = stack.pop() {
            for e in std::fs::read_dir(&dir)? {
                let e = e?;
                let name = e.file_name().to_string_lossy().to_string();
                let p = if rel.is_empty() {
                    name.clone()
                } else {
                    format!("{rel}/{name}")
                };
                let ft = e.file_type()?;
                if ft.is_dir() {
                    dirs.push(p.clone());
                    stack.push((e.path(), p));
                } else if ft.is_file() {
                    let m = e.metadata()?;
                    files.push(SourceFile {
                        path: p,
                        size: m.len(),
                        mtime: systime_ms(m.modified()),
                        ctime: systime_ms(m.created()),
                        compressed: None,
                    });
                }
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        dirs.sort();
        Ok((files, dirs))
    }
    fn open(&mut self, path: &str) -> io::Result<Box<dyn Read + '_>> {
        Ok(Box::new(std::fs::File::open(self.root.join(path))?))
    }
}

/// A zip archive (random access through `Read + Seek`; entries are streamed).
#[cfg(feature = "zip")]
pub struct ZipSource<R: io::Read + io::Seek> {
    archive: zip::ZipArchive<R>,
    index: HashMap<String, usize>,
    /// Common top-level folder stripped from every path (vaults are often zipped as `Vault/…`).
    prefix: String,
}

#[cfg(feature = "zip")]
impl<R: io::Read + io::Seek> ZipSource<R> {
    pub fn new(r: R) -> io::Result<ZipSource<R>> {
        let archive = zip::ZipArchive::new(r).map_err(io::Error::other)?;
        Ok(ZipSource {
            archive,
            index: HashMap::new(),
            prefix: String::new(),
        })
    }
}

/// Rejects absolute paths and `..` (zip-slip). Returns the normalised relative path.
pub fn safe_rel_path(raw: &str) -> Option<String> {
    let p = raw.replace('\\', "/");
    if p.starts_with('/') || (p.len() >= 2 && p.as_bytes()[1] == b':') {
        return None;
    }
    let mut parts = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => return None,
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

#[cfg(feature = "zip")]
impl<R: io::Read + io::Seek> ImportSource for ZipSource<R> {
    fn list(&mut self) -> io::Result<(Vec<SourceFile>, Vec<String>)> {
        let mut raw = Vec::new();
        for i in 0..self.archive.len() {
            let f = self.archive.by_index_raw(i).map_err(io::Error::other)?;
            let name = f.name().to_string();
            let dir = f.is_dir();
            let mtime = f.last_modified().and_then(|dt| {
                let (y, mo, d, h, mi, s) = (
                    dt.year() as i64,
                    dt.month() as i64,
                    dt.day() as i64,
                    dt.hour() as u64,
                    dt.minute() as u64,
                    dt.second() as u64,
                );
                if y < 1980 {
                    return None;
                }
                // days from civil
                let y2 = if mo <= 2 { y - 1 } else { y };
                let era = y2.div_euclid(400);
                let yoe = y2 - era * 400;
                let mp = (mo + 9) % 12;
                let doy = (153 * mp + 2) / 5 + d - 1;
                let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
                let days = (era * 146_097 + doe - 719_468) as u64;
                Some((days * 86400 + h * 3600 + mi * 60 + s) * 1000)
            });
            raw.push((i, name, dir, f.size(), f.compressed_size(), mtime));
        }
        // Strip a single common top-level folder.
        let tops: BTreeSet<String> = raw
            .iter()
            .filter_map(|r| safe_rel_path(&r.1))
            .map(|p| p.split('/').next().unwrap_or("").to_string())
            .collect();
        let all_nested = raw.iter().filter_map(|r| safe_rel_path(&r.1)).all(|p| {
            p.contains('/')
                || raw
                    .iter()
                    .any(|x| x.2 && safe_rel_path(&x.1).as_deref() == Some(p.as_str()))
        });
        if tops.len() == 1 && all_nested {
            let t = tops.into_iter().next().unwrap_or_default();
            if !t.ends_with(".md") {
                self.prefix = format!("{t}/");
            }
        }
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        for (i, name, dir, size, csize, mtime) in raw {
            let Some(p) = safe_rel_path(&name) else {
                files.push(SourceFile {
                    path: format!("\0unsafe:{name}"),
                    size: 0,
                    mtime: None,
                    ctime: None,
                    compressed: None,
                });
                continue;
            };
            let Some(p) = (if self.prefix.is_empty() {
                Some(p.clone())
            } else {
                p.strip_prefix(&self.prefix).map(String::from)
            }) else {
                continue;
            };
            if dir {
                dirs.push(p);
            } else {
                self.index.insert(p.clone(), i);
                files.push(SourceFile {
                    path: p,
                    size,
                    mtime,
                    ctime: None,
                    compressed: Some(csize),
                });
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        dirs.sort();
        Ok((files, dirs))
    }
    fn open(&mut self, path: &str) -> io::Result<Box<dyn Read + '_>> {
        // A source opened only to execute a plan made earlier hasn't been listed yet.
        if self.index.is_empty() {
            self.list()?;
        }
        let i = *self
            .index
            .get(path)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.to_string()))?;
        Ok(Box::new(
            self.archive.by_index(i).map_err(io::Error::other)?,
        ))
    }
}

// ------------------------------------------------------------------------ plan

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ObsidianSettings {
    pub attachment_folder_path: Option<String>,
    pub new_link_format: Option<String>,
    pub use_markdown_links: Option<bool>,
}

pub fn parse_app_json(bytes: &[u8]) -> ObsidianSettings {
    let v: serde_json::Value = serde_json::from_slice(bytes).unwrap_or_default();
    ObsidianSettings {
        attachment_folder_path: v
            .get("attachmentFolderPath")
            .and_then(|x| x.as_str())
            .map(String::from),
        new_link_format: v
            .get("newLinkFormat")
            .and_then(|x| x.as_str())
            .map(String::from),
        use_markdown_links: v.get("useMarkdownLinks").and_then(|x| x.as_bool()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum FileKind {
    Markdown,
    /// A `.md` that isn't valid UTF-8: stored as a read-only blob-backed note (D11).
    MarkdownBlob,
    Pdf,
    Media,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Conflict {
    Ask,
    Overwrite,
    KeepBoth,
    Skip,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Action {
    Create,
    /// Same path and same content already in the vault.
    SkipSame,
    /// Same path, different content: resolved by the policy (or asked).
    Conflict {
        #[serde(serialize_with = "ser_id")]
        existing: Id,
        resolution: Conflict,
    },
}

fn ser_id<S: serde::Serializer>(id: &Id, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&id.to_string())
}

#[derive(Clone, Debug, Serialize)]
pub struct PlanItem {
    pub path: String,
    pub kind: FileKind,
    pub visible: bool,
    pub size: u64,
    pub mtime: Option<u64>,
    pub ctime: Option<u64>,
    pub action: Action,
    /// Hidden by the attachment-folder rule (PDFs only).
    pub hidden_by_rule: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub notes: usize,
    pub pdfs: usize,
    pub pdfs_hidden: usize,
    pub images: usize,
    pub other_media: usize,
    pub folders: usize,
    pub unchanged: usize,
    pub conflicts: usize,
    pub skipped: Vec<(String, String)>,
    pub unresolved: Vec<(String, String)>,
    pub collisions: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub settings: ObsidianSettings,
    pub folders: Vec<String>,
    pub items: Vec<PlanItem>,
    pub report: Report,
}

#[derive(Clone, Debug)]
pub struct PlanOptions {
    pub hide_pdfs_in_attachment_folder: bool,
    pub conflict: Conflict,
    /// Refuse sources expanding beyond this many bytes (zip bombs).
    pub max_total_bytes: u64,
    /// Entries whose size / compressed size exceeds this (and are > 16 MiB) are skipped.
    pub max_ratio: u64,
}

impl Default for PlanOptions {
    fn default() -> Self {
        PlanOptions {
            hide_pdfs_in_attachment_folder: true,
            conflict: Conflict::Ask,
            max_total_bytes: 200 << 30,
            max_ratio: 1000,
        }
    }
}

/// Existing vault content the planner compares against (idempotency).
pub trait Existing {
    fn state(&self) -> &MetaState;
    /// Exact text bytes of an existing markdown note.
    fn text(&mut self, id: Id) -> Option<Vec<u8>>;
}

const SKIP_DIRS: &[&str] = &[".obsidian", ".git", ".trash", "__MACOSX"];
const SKIP_FILES: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini"];

pub fn skip_reason(path: &str) -> Option<&'static str> {
    let segs: Vec<&str> = path.split('/').collect();
    if segs[..segs.len() - 1]
        .iter()
        .chain(std::iter::once(segs.last().unwrap_or(&"")))
        .any(|s| SKIP_DIRS.contains(s))
        && segs.iter().any(|s| SKIP_DIRS.contains(s))
    {
        return Some("excluded folder");
    }
    if SKIP_FILES.contains(segs.last().unwrap_or(&""))
        || segs.last().map(|s| s.starts_with("._")).unwrap_or(false)
    {
        return Some("system file");
    }
    None
}

pub fn is_md(path: &str) -> bool {
    path.len() >= 3 && path[path.len() - 3..].eq_ignore_ascii_case(".md")
}
pub fn is_pdf(path: &str) -> bool {
    path.len() >= 4 && path[path.len() - 4..].eq_ignore_ascii_case(".pdf")
}
pub fn is_image(path: &str) -> bool {
    let l = path.to_ascii_lowercase();
    [
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".heic", ".heif", ".bmp", ".avif",
        ".tif", ".tiff",
    ]
    .iter()
    .any(|e| l.ends_with(e))
}

pub fn mime_for_name(name: &str) -> Option<&'static str> {
    let l = name.to_ascii_lowercase();
    let ext = l.rsplit('.').next()?;
    Some(match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        "tif" | "tiff" => "image/tiff",
        "pdf" => "application/pdf",
        "md" => "text/markdown",
        "txt" => "text/plain",
        "canvas" | "json" => "application/json",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "zip" => "application/zip",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        _ => return None,
    })
}

/// Blob facts from a file's first bytes (dimensions via `imagesize`; EXIF orientation for JPEG).
pub fn blob_info_from_header(name: &str, header: &[u8], size: u64) -> BlobInfo {
    let mut bi = BlobInfo {
        size,
        mime: mime_for_name(name).map(String::from),
        ..Default::default()
    };
    if is_image(name) {
        if let Ok(d) = imagesize::blob_size(header) {
            bi.width = Some(d.width as u32);
            bi.height = Some(d.height as u32);
        }
        bi.orientation = jpeg_orientation(header);
    }
    bi
}

/// EXIF orientation (1–8) of a JPEG, from its first bytes.
pub fn jpeg_orientation(b: &[u8]) -> Option<u8> {
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let marker = b[i + 1];
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if marker == 0xE1 && i + 4 + len <= b.len() + 2 && b.get(i + 4..i + 10) == Some(b"Exif\0\0")
        {
            let t = &b[i + 10..(i + 2 + len).min(b.len())];
            if t.len() < 8 {
                return None;
            }
            let le = &t[0..2] == b"II";
            let u16at = |o: usize| -> Option<u16> {
                t.get(o..o + 2).map(|x| {
                    if le {
                        u16::from_le_bytes([x[0], x[1]])
                    } else {
                        u16::from_be_bytes([x[0], x[1]])
                    }
                })
            };
            let u32at = |o: usize| -> Option<u32> {
                t.get(o..o + 4).map(|x| {
                    if le {
                        u32::from_le_bytes([x[0], x[1], x[2], x[3]])
                    } else {
                        u32::from_be_bytes([x[0], x[1], x[2], x[3]])
                    }
                })
            };
            let ifd = u32at(4)? as usize;
            let n = u16at(ifd)? as usize;
            for k in 0..n {
                let e = ifd + 2 + k * 12;
                if u16at(e)? == 0x0112 {
                    let v = u16at(e + 8)?;
                    return if (1..=8).contains(&v) {
                        Some(v as u8)
                    } else {
                        None
                    };
                }
            }
            return None;
        }
        if marker == 0xDA {
            return None;
        }
        i += 2 + len;
    }
    None
}

fn parent_dir(p: &str) -> &str {
    p.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

/// Is `path` (a PDF) inside the configured attachment folder?
fn in_attachment_folder(path: &str, setting: Option<&str>) -> bool {
    let Some(s) = setting.map(str::trim) else {
        return false;
    };
    if s.is_empty() || s == "/" || s == "." || s == "./" {
        return false; // vault root or "same folder as the note": no dedicated folder
    }
    let dir = parent_dir(path);
    if let Some(rel) = s.strip_prefix("./") {
        // "./sub": a subfolder named `sub` next to notes.
        let rel = rel.trim_end_matches('/');
        return dir == rel
            || dir.ends_with(&format!("/{rel}"))
            || dir.contains(&format!("/{rel}/"))
            || dir.starts_with(&format!("{rel}/"));
    }
    let f = s.trim_matches('/');
    dir == f || dir.starts_with(&format!("{f}/"))
}

/// Builds the plan (this is also the dry run). Reads `.obsidian/app.json` and every note's text
/// (for idempotency and the unresolved-links report); blobs are only read when their path
/// already exists in the vault.
pub fn plan(
    src: &mut dyn ImportSource,
    existing: &mut dyn Existing,
    opts: &PlanOptions,
) -> io::Result<Plan> {
    let (files, dirs) = src.list()?;
    let mut report = Report::default();
    let settings = match files.iter().find(|f| f.path == ".obsidian/app.json") {
        Some(f) => parse_app_json(&src.read_all(&f.path.clone())?),
        None => ObsidianSettings::default(),
    };
    let mut total: u64 = 0;
    let mut items = Vec::new();
    let mut folders: BTreeSet<String> = BTreeSet::new();
    let mut skipped_dirs: BTreeSet<String> = BTreeSet::new();
    for d in &dirs {
        if skip_reason(d).is_some() {
            let top = d.split('/').scan(String::new(), |acc, s| {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(s);
                Some(acc.clone())
            });
            if let Some(t) = top
                .into_iter()
                .find(|p| SKIP_DIRS.contains(&p.rsplit('/').next().unwrap_or("")))
            {
                skipped_dirs.insert(t);
            }
            continue;
        }
        folders.insert(d.clone());
    }
    let st = existing.state().clone();
    for f in files {
        if let Some(raw) = f.path.strip_prefix("\0unsafe:") {
            report
                .skipped
                .push((raw.to_string(), "unsafe path (absolute or ..)".into()));
            continue;
        }
        if let Some(why) = skip_reason(&f.path) {
            if why == "excluded folder" {
                let top = f.path.split('/').scan(String::new(), |acc, s| {
                    if !acc.is_empty() {
                        acc.push('/');
                    }
                    acc.push_str(s);
                    Some(acc.clone())
                });
                if let Some(t) = top
                    .into_iter()
                    .find(|p| SKIP_DIRS.contains(&p.rsplit('/').next().unwrap_or("")))
                {
                    skipped_dirs.insert(t);
                }
            } else {
                report.skipped.push((f.path.clone(), why.into()));
            }
            continue;
        }
        if let Some(c) = f.compressed {
            if c > 0 && f.size > (16 << 20) && f.size / c > opts.max_ratio {
                report
                    .skipped
                    .push((f.path.clone(), "suspicious compression ratio".into()));
                continue;
            }
        }
        total += f.size;
        if total > opts.max_total_bytes {
            return Err(io::Error::other("import exceeds the maximum total size"));
        }
        // Implied folders.
        let mut d = parent_dir(&f.path).to_string();
        while !d.is_empty() {
            folders.insert(d.clone());
            d = parent_dir(&d).to_string();
        }
        let (kind, visible, hidden_by_rule) = if is_md(&f.path) {
            (FileKind::Markdown, true, false)
        } else if is_pdf(&f.path) {
            let hidden = opts.hide_pdfs_in_attachment_folder
                && in_attachment_folder(&f.path, settings.attachment_folder_path.as_deref());
            (FileKind::Pdf, !hidden, hidden)
        } else {
            (FileKind::Media, false, false)
        };
        items.push(PlanItem {
            path: f.path,
            kind,
            visible,
            size: f.size,
            mtime: f.mtime,
            ctime: f.ctime,
            action: Action::Create,
            hidden_by_rule,
        });
    }
    for d in skipped_dirs {
        report
            .skipped
            .push((format!("{d}/"), "excluded folder".into()));
    }
    // Read notes: UTF-8 validity, idempotency, links.
    let mut ix = ResolveIndex::build(&st);
    for it in &items {
        if st.by_path(&it.path).is_none() {
            ix.insert(Id::derive(&[it.path.as_bytes()]), &it.path);
        }
    }
    let mut unresolved = Vec::new();
    for it in items.iter_mut() {
        let existing_id = st.by_path(&it.path);
        match it.kind {
            FileKind::Markdown => {
                let bytes = src.read_all(&it.path)?;
                let text = match String::from_utf8(bytes.clone()) {
                    Ok(t) => Some(t),
                    Err(_) => {
                        it.kind = FileKind::MarkdownBlob;
                        None
                    }
                };
                if let Some(t) = &text {
                    let folder = parent_dir(&it.path);
                    for l in extract(t).links {
                        if l.target.trim().is_empty() || crate::links::is_external(&l.target) {
                            continue;
                        }
                        if ix.resolve(&l.target, l.syntax, folder).is_none() {
                            let shown = if l.syntax == Syntax::Wiki {
                                format!("[[{}]]", l.target)
                            } else {
                                format!("({})", l.raw_target)
                            };
                            unresolved.push((it.path.clone(), shown));
                        }
                    }
                }
                if let Some(id) = existing_id {
                    let e = st.get(&id).cloned();
                    let same = match (&e, &text) {
                        (Some(e), Some(_)) if e.kind == KIND_MARKDOWN && e.blob.is_none() => {
                            existing.text(id).as_deref() == Some(&bytes[..])
                        }
                        (Some(e), None) => e.blob == Some(Hash::of(&bytes)),
                        _ => false,
                    };
                    it.action = if same {
                        Action::SkipSame
                    } else {
                        Action::Conflict {
                            existing: id,
                            resolution: opts.conflict,
                        }
                    };
                }
            }
            _ => {
                if let Some(id) = existing_id {
                    let mut r = src.open(&it.path)?;
                    let h = hash_reader(&mut r)?;
                    let same = st
                        .get(&id)
                        .map(|e| e.blob == Some(h) && !e.is_folder())
                        .unwrap_or(false);
                    it.action = if same {
                        Action::SkipSame
                    } else {
                        Action::Conflict {
                            existing: id,
                            resolution: opts.conflict,
                        }
                    };
                }
            }
        }
        match it.action {
            Action::SkipSame => report.unchanged += 1,
            Action::Conflict { .. } => report.conflicts += 1,
            Action::Create => match it.kind {
                FileKind::Markdown | FileKind::MarkdownBlob => report.notes += 1,
                FileKind::Pdf => {
                    report.pdfs += 1;
                    if it.hidden_by_rule {
                        report.pdfs_hidden += 1;
                    }
                }
                FileKind::Media if is_image(&it.path) => report.images += 1,
                FileKind::Media => report.other_media += 1,
            },
        }
    }
    report.unresolved = unresolved;
    // Case-insensitive name collisions (fine in Jess, D8; flagged for portable exports).
    let mut seen: HashMap<String, Vec<String>> = HashMap::new();
    for p in items
        .iter()
        .map(|i| i.path.clone())
        .chain(folders.iter().cloned())
    {
        seen.entry(crate::names::lookup_key(&p))
            .or_default()
            .push(p);
    }
    let mut coll: Vec<String> = seen
        .into_values()
        .filter(|v| v.len() > 1)
        .map(|mut v| {
            v.sort();
            v.join(" / ")
        })
        .collect();
    coll.sort();
    report.collisions = coll;
    report.folders = folders.iter().filter(|f| st.by_path(f).is_none()).count();
    Ok(Plan {
        settings,
        folders: folders.into_iter().collect(),
        items,
        report,
    })
}

pub fn hash_reader(r: &mut dyn Read) -> io::Result<Hash> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let k = r.read(&mut buf)?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
    }
    Ok(Hash(h.finalize().into()))
}

// ------------------------------------------------------------------------ execute

/// Where an import lands: a client (local ops, synced) or the server (server-authored ops).
pub trait ImportSink {
    fn new_id(&mut self) -> Id;
    /// Applies one atomic group of meta ops.
    fn meta(&mut self, ops: Vec<MetaOp>) -> io::Result<()>;
    /// Creates a note's text doc (a single insert) or, when `existing`, applies a minimal diff.
    fn doc(&mut self, entry: Id, text: &str, existing: bool) -> io::Result<()>;
    /// Streams a file into local blob storage, returning its hash and facts.
    fn blob(&mut self, name: &str, r: &mut dyn Read, size: u64) -> io::Result<(Hash, BlobInfo)>;
}

fn keep_both_name(name: &str) -> String {
    let (stem, ext) = crate::names::split_ext(name, false);
    format!("{stem} (imported){ext}")
}

/// Executes a plan in batches of `BATCH` files (one op group each). Unresolved `Ask` conflicts
/// are an error: the caller must resolve them first.
pub fn execute(
    plan: &Plan,
    src: &mut dyn ImportSource,
    existing: &MetaState,
    sink: &mut dyn ImportSink,
    mut progress: impl FnMut(usize, usize),
) -> io::Result<Report> {
    if plan.items.iter().any(|i| {
        matches!(
            i.action,
            Action::Conflict {
                resolution: Conflict::Ask,
                ..
            }
        )
    }) {
        return Err(io::Error::other(
            "unresolved conflicts: choose overwrite, keep both or skip",
        ));
    }
    let mut folder_ids: BTreeMap<String, Id> = BTreeMap::new();
    // Folders, parents first.
    let mut fs: Vec<&String> = plan.folders.iter().collect();
    fs.sort_by_key(|f| (f.matches('/').count(), (*f).clone()));
    let mut group = Vec::new();
    for f in fs {
        if let Some(id) = existing
            .by_path(f)
            .filter(|id| existing.get(id).map(|e| e.is_folder()).unwrap_or(false))
        {
            folder_ids.insert(f.clone(), id);
            continue;
        }
        let id = sink.new_id();
        let parent = folder_ids.get(parent_dir(f)).copied();
        let name = f.rsplit('/').next().unwrap_or(f).to_string();
        group.push(MetaOp::Create {
            id,
            kind: KIND_FOLDER.into(),
            parent,
            name,
            tree_visible: true,
            blob: None,
            blob_info: None,
            created_at: None,
            modified_at: None,
            props: vec![],
        });
        folder_ids.insert(f.clone(), id);
        if group.len() >= BATCH {
            sink.meta(std::mem::take(&mut group))?;
        }
    }
    if !group.is_empty() {
        sink.meta(group)?;
    }
    // Vault settings from .obsidian/app.json.
    let mut sets = Vec::new();
    if let Some(v) = &plan.settings.attachment_folder_path {
        sets.push(MetaOp::SetProp {
            id: VAULT_SETTINGS_ID,
            key: "attachmentFolderPath".into(),
            value: Some(PropValue::str(v)),
        });
    }
    if let Some(v) = &plan.settings.new_link_format {
        sets.push(MetaOp::SetProp {
            id: VAULT_SETTINGS_ID,
            key: "newLinkFormat".into(),
            value: Some(PropValue::str(v)),
        });
    }
    if let Some(v) = plan.settings.use_markdown_links {
        sets.push(MetaOp::SetProp {
            id: VAULT_SETTINGS_ID,
            key: "useMarkdownLinks".into(),
            value: Some(PropValue::bool(v)),
        });
    }
    let mut report = plan.report.clone();
    if !sets.is_empty() {
        // Non-fatal: e.g. a client that hasn't synced the vault record yet.
        if let Err(e) = sink.meta(sets) {
            report
                .warnings
                .push(format!("attachment settings not applied: {e}"));
        }
    }
    let todo: Vec<&PlanItem> = plan
        .items
        .iter()
        .filter(|i| {
            !matches!(
                i.action,
                Action::SkipSame
                    | Action::Conflict {
                        resolution: Conflict::Skip,
                        ..
                    }
            )
        })
        .collect();
    let total = todo.len();
    let mut done = 0;
    for batch in todo.chunks(BATCH) {
        let mut ops = Vec::new();
        let mut docs: Vec<(Id, String, bool)> = Vec::new();
        for it in batch {
            let parent = folder_ids.get(parent_dir(&it.path)).copied();
            let mut name = it.path.rsplit('/').next().unwrap_or(&it.path).to_string();
            let overwrite = match &it.action {
                Action::Conflict {
                    existing,
                    resolution: Conflict::Overwrite,
                } => Some(*existing),
                Action::Conflict {
                    resolution: Conflict::KeepBoth,
                    ..
                } => {
                    name = keep_both_name(&name);
                    None
                }
                _ => None,
            };
            let text = if it.kind == FileKind::Markdown {
                Some(String::from_utf8(src.read_all(&it.path)?).map_err(io::Error::other)?)
            } else {
                None
            };
            let (blob, info) = if it.kind == FileKind::Markdown {
                (None, None)
            } else {
                let mut r = src.open(&it.path)?;
                let (h, bi) = sink.blob(&name, &mut r, it.size)?;
                (Some(h), Some(bi))
            };
            match overwrite {
                Some(id) => {
                    if let Some(t) = text {
                        docs.push((id, t, true));
                    } else if let Some(h) = blob {
                        ops.push(MetaOp::SetBlob {
                            id,
                            blob: h,
                            blob_info: info,
                        });
                    }
                }
                None => {
                    let id = sink.new_id();
                    let kind = match it.kind {
                        FileKind::Markdown | FileKind::MarkdownBlob => KIND_MARKDOWN,
                        FileKind::Pdf => KIND_PDF,
                        FileKind::Media => KIND_MEDIA,
                    };
                    ops.push(MetaOp::Create {
                        id,
                        kind: kind.into(),
                        parent,
                        name,
                        tree_visible: it.visible,
                        blob,
                        blob_info: info,
                        created_at: it.ctime.or(it.mtime),
                        modified_at: it.mtime,
                        props: vec![],
                    });
                    if let Some(t) = text {
                        docs.push((id, t, false));
                    }
                }
            }
        }
        if !ops.is_empty() {
            sink.meta(ops)?;
        }
        for (id, t, ex) in docs {
            sink.doc(id, &t, ex)?;
        }
        done += batch.len();
        progress(done, total);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zip_slip_and_skips() {
        assert_eq!(safe_rel_path("a/../b"), None);
        assert_eq!(safe_rel_path("/etc/passwd"), None);
        assert_eq!(safe_rel_path("C:/x"), None);
        assert_eq!(safe_rel_path("a\\b.md").as_deref(), Some("a/b.md"));
        assert_eq!(skip_reason(".obsidian/app.json"), Some("excluded folder"));
        assert_eq!(skip_reason("a/.git/config"), Some("excluded folder"));
        assert_eq!(skip_reason("a/.DS_Store"), Some("system file"));
        assert_eq!(skip_reason("a/b.md"), None);
        assert!(in_attachment_folder(
            "attachments/x.pdf",
            Some("attachments")
        ));
        assert!(in_attachment_folder(
            "attachments/sub/x.pdf",
            Some("/attachments/")
        ));
        assert!(!in_attachment_folder("notes/x.pdf", Some("attachments")));
        assert!(in_attachment_folder("notes/assets/x.pdf", Some("./assets")));
        assert!(!in_attachment_folder("notes/x.pdf", Some("./")));
    }
    #[test]
    fn exif_orientation() {
        // Minimal JPEG: SOI, APP1 Exif (big-endian) with Orientation = 6.
        let mut t = b"MM\0\x2a\0\0\0\x08".to_vec();
        t.extend_from_slice(&[0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0]);
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE1];
        let len = (2 + 6 + t.len()) as u16;
        j.extend_from_slice(&len.to_be_bytes());
        j.extend_from_slice(b"Exif\0\0");
        j.extend_from_slice(&t);
        assert_eq!(jpeg_orientation(&j), Some(6));
    }
}
