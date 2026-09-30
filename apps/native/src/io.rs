//! Import and export on native platforms (DESIGN §12): folders and zips straight from disk, the
//! same core planner as every other path, executed as ordinary local ops (so they sync).

use crate::{now_ms, Native};
use jess_core::export::ContentSource;
use jess_core::import::{
    self, Action, Conflict, Existing, ImportSink, ImportSource, Plan, PlanOptions,
};
use jess_core::model::{BlobInfo, KIND_MARKDOWN};
use jess_core::ops::MetaOp;
use jess_core::projection::{project, Options, Profile};
use jess_core::state::MetaState;
use jess_core::{Hash, Id};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum Source {
    Folder(PathBuf),
    Zip(PathBuf),
    /// An already-open, seekable zip (an Android `content://` document). Opened again for the run
    /// by cloning the descriptor and rewinding it.
    ZipFile(Arc<std::fs::File>),
}

pub struct ImportState {
    plan: Plan,
    source: Source,
}

fn open_source(s: &Source) -> io::Result<Box<dyn ImportSource>> {
    Ok(match s {
        Source::Folder(p) => Box::new(import::FolderSource { root: p.clone() }),
        Source::Zip(p) => Box::new(import::ZipSource::new(io::BufReader::new(
            std::fs::File::open(p)?,
        ))?),
        Source::ZipFile(f) => {
            let mut f = f.try_clone()?;
            f.seek(io::SeekFrom::Start(0))?;
            Box::new(import::ZipSource::new(io::BufReader::new(f))?)
        }
    })
}

struct MapExisting<'a> {
    st: &'a MetaState,
    texts: HashMap<Id, Vec<u8>>,
}

impl Existing for MapExisting<'_> {
    fn state(&self) -> &MetaState {
        self.st
    }
    fn text(&mut self, id: Id) -> Option<Vec<u8>> {
        self.texts.get(&id).cloned()
    }
}

struct Sink {
    n: Native,
}

impl ImportSink for Sink {
    fn new_id(&mut self) -> Id {
        Id::new_v7(now_ms(), rand::random())
    }
    fn meta(&mut self, ops: Vec<MetaOp>) -> io::Result<()> {
        if self.n.inner.import_cancel.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let mut st = self.n.inner.st.lock().expect("lock");
        let o = st
            .client
            .local_meta(ops, now_ms())
            .map_err(|r| io::Error::other(format!("{r:?}")))?;
        self.n.apply_locked(&mut st, o);
        Ok(())
    }
    fn doc(&mut self, entry: Id, text: &str, existing: bool) -> io::Result<()> {
        let d = jess_core::doc::new_doc(rand::random::<u32>() as u64);
        let u = if existing {
            for r in self.n.doc_updates_of(entry) {
                let _ = jess_core::doc::apply(&d, &r);
            }
            jess_core::doc::set_text(&d, text)
        } else {
            jess_core::doc::insert(&d, 0, text)
        };
        let mut st = self.n.inner.st.lock().expect("lock");
        let o = st.client.local_doc_update(entry, "body", u, now_ms());
        self.n.apply_locked(&mut st, o);
        Ok(())
    }
    fn blob(&mut self, name: &str, r: &mut dyn Read, _size: u64) -> io::Result<(Hash, BlobInfo)> {
        let v = self.n.ingest_reader(name, r).map_err(io::Error::other)?;
        let h = Hash::parse_hex(v["hash"].as_str().unwrap_or(""))
            .ok_or_else(|| io::Error::other("bad hash"))?;
        let size = v["size"].as_u64().unwrap_or(0);
        let i = &v["info"];
        Ok((
            h,
            BlobInfo {
                size,
                mime: i["mime"].as_str().map(String::from),
                width: i["width"].as_u64().map(|x| x as u32),
                height: i["height"].as_u64().map(|x| x as u32),
                orientation: i["orientation"].as_u64().map(|x| x as u8),
            },
        ))
    }
}

struct Content {
    n: Native,
}

impl ContentSource for Content {
    fn text(&mut self, id: Id) -> io::Result<Vec<u8>> {
        Ok(self.n.doc_text_of(id).into_bytes())
    }
    fn blob(&mut self, h: Hash) -> io::Result<(Box<dyn Read + '_>, u64)> {
        let size = self
            .n
            .blob_size(&h)
            .ok_or_else(|| io::Error::other(format!("blob {h} unknown")))?;
        if self.n.blob_is_local(&h) {
            let f = std::fs::File::open(self.n.inner.blobs.path(&h))?;
            return Ok((Box::new(io::BufReader::new(f)), size));
        }
        // Not on this device: fetch it (not kept; exports don't change what's offline).
        let (code, body) = self
            .n
            .http_get(&format!("/api/blobs/{h}"), None)
            .map_err(io::Error::other)?;
        if code != 200 || body.len() as u64 != size {
            return Err(io::Error::other(format!("blob {h} unavailable ({code})")));
        }
        Ok((Box::new(io::Cursor::new(body)), size))
    }
}

impl Native {
    fn view_clone(&self) -> MetaState {
        self.inner.st.lock().expect("lock").client.view().clone()
    }

    /// Plans an import (the dry run): `{settings, folders, items, report}`.
    pub fn import_plan(
        &self,
        source: Source,
        hide_pdfs: bool,
        conflict: &str,
    ) -> Result<Value, String> {
        let conflict = match conflict {
            "overwrite" => Conflict::Overwrite,
            "keepBoth" => Conflict::KeepBoth,
            "skip" => Conflict::Skip,
            _ => Conflict::Ask,
        };
        let opts = PlanOptions {
            conflict,
            hide_pdfs_in_attachment_folder: hide_pdfs,
            ..Default::default()
        };
        let view = self.view_clone();
        let mut src = open_source(&source).map_err(|e| e.to_string())?;
        // Existing notes at the same paths: their exact text decides "unchanged" vs conflict.
        let (files, _) = src.list().map_err(|e| e.to_string())?;
        let mut texts = HashMap::new();
        for f in files {
            if let Some(id) = view.by_path(&f.path) {
                if view
                    .get(&id)
                    .map(|e| e.kind == KIND_MARKDOWN && e.blob.is_none())
                    .unwrap_or(false)
                {
                    texts.insert(id, self.doc_text_of(id).into_bytes());
                }
            }
        }
        let mut ex = MapExisting { st: &view, texts };
        let plan = import::plan(src.as_mut(), &mut ex, &opts).map_err(|e| e.to_string())?;
        let v = serde_json::to_value(&plan).map_err(|e| e.to_string())?;
        *self.inner.import.lock().expect("lock") = Some(ImportState { plan, source });
        Ok(v)
    }

    /// Runs the planned import. `resolutions` maps paths to `Overwrite`/`KeepBoth`/`Skip`;
    /// `apply_all` answers every remaining conflict.
    pub fn import_run(
        &self,
        resolutions: &HashMap<String, String>,
        apply_all: Option<&str>,
    ) -> Result<Value, String> {
        let Some(ImportState { mut plan, source }) = self.inner.import.lock().expect("lock").take()
        else {
            return Err("no import planned".into());
        };
        let pick = |s: &str| match s {
            "Overwrite" => Some(Conflict::Overwrite),
            "KeepBoth" => Some(Conflict::KeepBoth),
            "Skip" => Some(Conflict::Skip),
            _ => None,
        };
        for it in &mut plan.items {
            if let Action::Conflict { resolution, .. } = &mut it.action {
                if let Some(r) = resolutions
                    .get(&it.path)
                    .and_then(|s| pick(s))
                    .or_else(|| apply_all.and_then(pick))
                {
                    *resolution = r;
                }
            }
        }
        self.inner.import_cancel.store(false, Ordering::SeqCst);
        let mut src = open_source(&source).map_err(|e| e.to_string())?;
        let view = self.view_clone();
        let mut sink = Sink { n: self.clone() };
        let n = self.clone();
        let r = import::execute(&plan, src.as_mut(), &view, &mut sink, |done, total| {
            n.emit(json!({"ev": "progress", "task": "import", "done": done, "total": total}));
        });
        match r {
            Ok(rep) => {
                let mut v = serde_json::to_value(&rep).map_err(|e| e.to_string())?;
                v["cancelled"] = json!(false);
                Ok(v)
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(json!({"cancelled": true})),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn import_cancel(&self) {
        self.inner.import_cancel.store(true, Ordering::SeqCst);
    }

    /// Exports the vault to a zip file or into a folder (`EXPORT-REPORT.txt` when names changed).
    pub fn export_to(&self, dest: &Path, portable: bool, zip: bool) -> Result<(), String> {
        if zip {
            let tmp = dest.with_extension("zip.partial");
            let f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
            self.export_zip_into(f, portable)?;
            std::fs::rename(&tmp, dest).map_err(|e| e.to_string())
        } else {
            let p = self.projection(portable);
            let mut src = Content { n: self.clone() };
            jess_core::export::write_folder(&p, &mut src, dest).map_err(|e| e.to_string())
        }
    }

    /// Exports the vault as a zip into an open, empty file (an Android `content://` document the
    /// user created; there's no rename step there).
    pub fn export_zip_into(&self, f: std::fs::File, portable: bool) -> Result<(), String> {
        let p = self.projection(portable);
        let mut src = Content { n: self.clone() };
        let n = self.clone();
        let w = jess_core::export::write_zip(&p, &mut src, io::BufWriter::new(f), |done, total| {
            n.emit(json!({"ev": "progress", "task": "export", "done": done, "total": total}));
        })
        .map_err(|e| e.to_string())?;
        let f = w.into_inner().map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())
    }

    fn projection(&self, portable: bool) -> jess_core::projection::Projection {
        project(
            &self.view_clone(),
            Options {
                profile: if portable {
                    Profile::Portable
                } else {
                    Profile::Exact
                },
                include_trash: false,
            },
        )
    }
}
