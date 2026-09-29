//! `jess integrity-check` (DESIGN §15). Read-only; returns a list of problems.

use crate::blobfs::BlobFs;
use crate::db;
use crate::error::Result;
use jess_core::doc as ydoc;
use jess_core::links::extract;
use jess_core::model::{KIND_MARKDOWN, SLOT_BODY};
use jess_core::resolve::ResolveIndex;
use jess_core::state::MetaState;
use jess_core::{Hash, Id};
use rusqlite::Connection;
use std::collections::HashSet;

pub struct Options {
    pub hashes: bool,
}

pub fn check(conn: &Connection, fs: &BlobFs, opts: &Options) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    let ic: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if ic != "ok" {
        problems.push(format!("sqlite integrity_check: {ic}"));
    }
    let entries = db::load_entries(conn)?;
    let st = MetaState::from_entries(entries);
    problems.extend(st.check_invariants());
    let ix = ResolveIndex::build(&st);
    // Docs decode, and the links index matches the text.
    let docs: Vec<(Vec<u8>, String)> = {
        let mut s = conn.prepare("SELECT DISTINCT entry_id, slot FROM doc_updates")?;
        let v = s
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        v
    };
    for (eid, slot) in docs {
        let Some(id) = Id::from_slice(&eid) else {
            problems.push("doc row with malformed entry id".into());
            continue;
        };
        let rows: Vec<Vec<u8>> = {
            let mut s = conn.prepare(
                "SELECT \"update\" FROM doc_updates WHERE entry_id = ?1 AND slot = ?2 ORDER BY seq",
            )?;
            let v = s
                .query_map(rusqlite::params![eid, slot], |r| r.get(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            v
        };
        let d = ydoc::new_doc(1);
        let mut ok = true;
        for r in &rows {
            if ydoc::apply(&d, r).is_err() {
                problems.push(format!("doc {id} slot {slot}: an update fails to decode"));
                ok = false;
                break;
            }
        }
        if !ok {
            continue;
        }
        if ydoc::has_pending(&d) {
            problems.push(format!(
                "doc {id} slot {slot}: updates with missing dependencies"
            ));
        }
        match st.get(&id) {
            None => problems.push(format!("doc {id} has no entry")),
            Some(e) if e.purged => problems.push(format!("doc {id} belongs to a purged entry")),
            Some(e) if e.kind == KIND_MARKDOWN && slot == SLOT_BODY => {
                let text = ydoc::text(&d);
                let folder = st.folder_of(id);
                let want: Vec<(String, Option<Id>)> = extract(&text)
                    .links
                    .iter()
                    .map(|l| {
                        (
                            l.target.clone(),
                            ix.resolve(&l.target, l.syntax, &folder).map(|r| r.id),
                        )
                    })
                    .collect();
                let mut s = conn.prepare(
                    "SELECT target, resolved FROM links WHERE src = ?1 AND slot = ?2 ORDER BY ord",
                )?;
                let got: Vec<(String, Option<Id>)> = s
                    .query_map(rusqlite::params![eid, slot], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<Vec<u8>>>(1)?
                                .and_then(|b| Id::from_slice(&b)),
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                if got != want {
                    problems.push(format!("links index out of date for {id}"));
                }
            }
            _ => {}
        }
    }
    // Blobs referenced by non-purged entries exist with the right size (and hash).
    let mut seen = HashSet::new();
    for e in st.iter().filter(|e| !e.purged) {
        let Some(h) = e.blob else { continue };
        if !seen.insert(h) {
            continue;
        }
        let row: Option<(i64, i64)> = conn
            .query_row(
                "SELECT present, size FROM blobs WHERE hash = ?1",
                [h.0.to_vec()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        match row {
            None => problems.push(format!("blob {h} referenced by {} has no row", e.id)),
            Some((0, _)) => {} // not uploaded yet: reported as `missing` in admin status, not corruption
            Some((_, size)) => match fs.size(&h) {
                None => problems.push(format!("blob {h} marked present but file missing")),
                Some(s) if s as i64 != size => {
                    problems.push(format!("blob {h} size {s} != recorded {size}"))
                }
                Some(_) => {
                    if opts.hashes && !fs.verify(&h).unwrap_or(false) {
                        problems.push(format!("blob {h} content does not match its hash"));
                    }
                }
            },
        }
    }
    // Stray temp files (not belonging to an upload session).
    let uploads: HashSet<String> = {
        let mut s = conn.prepare("SELECT upload_id FROM uploads")?;
        let v = s
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        v
    };
    for p in fs.tmp_files() {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !uploads.contains(&name) {
            problems.push(format!("stray temp file {}", p.display()));
        }
    }
    let _ = Hash::default();
    Ok(problems)
}
