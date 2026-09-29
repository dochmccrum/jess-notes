//! Round-trip (DESIGN §12.3): fixture vault → import (server path from a folder and from a zip;
//! client path through a real sync client) → export (folder and zip) → byte-for-byte equal,
//! ignoring skipped items. Re-importing is idempotent.

mod common;
use common::inproc::{ClientSink, InProc};
use jess_core::export::{write_folder, write_zip};
use jess_core::import::{self, Existing, FolderSource, PlanOptions, ZipSource};
use jess_core::projection::{project, Options, Profile};
use jess_core::state::MetaState;
use jess_core::Id;
use jess_server::blobfs::BlobFs;
use jess_server::db;
use jess_server::engine::{Engine, EngineConfig};
use jess_server::vault_io::{ServerSink, Snapshot};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

type Tree = (BTreeMap<String, Vec<u8>>, BTreeSet<String>);

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/vault")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), &t).unwrap();
        }
    }
}

/// The fixture plus what git can't hold: empty folders and a 5 MB note.
fn prepare() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    copy_dir(&fixture(), d.path());
    std::fs::create_dir_all(d.path().join("Empty/Nested empty")).unwrap();
    let mut big = String::with_capacity(5 << 20);
    let mut i = 0;
    while big.len() < 5 << 20 {
        big.push_str(&format!(
            "Line {i} with a [[Welcome]] link, some $x_{i}$ maths and #tag{}\n",
            i % 50
        ));
        i += 1;
    }
    std::fs::write(d.path().join("Big note.md"), big).unwrap();
    d
}

fn read_tree(root: &Path) -> Tree {
    let mut files = BTreeMap::new();
    let mut dirs = BTreeSet::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((p, rel)) = stack.pop() {
        for e in std::fs::read_dir(&p).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().to_string();
            let r = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if import::skip_reason(&r).is_some() {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                dirs.insert(r.clone());
                stack.push((e.path(), r));
            } else {
                files.insert(r, std::fs::read(e.path()).unwrap());
            }
        }
    }
    (files, dirs)
}

fn zip_tree(bytes: &[u8]) -> Tree {
    let mut a = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut files = BTreeMap::new();
    let mut dirs = BTreeSet::new();
    for i in 0..a.len() {
        let mut f = a.by_index(i).unwrap();
        let name = f.name().trim_end_matches('/').to_string();
        if f.is_dir() {
            dirs.insert(name);
        } else {
            let mut v = Vec::new();
            f.read_to_end(&mut v).unwrap();
            files.insert(name, v);
        }
    }
    (files, dirs)
}

fn assert_same(want: &Tree, got: &Tree, what: &str) {
    let wk: BTreeSet<&String> = want.0.keys().collect();
    let gk: BTreeSet<&String> = got.0.keys().collect();
    assert_eq!(
        wk.difference(&gk).collect::<Vec<_>>(),
        Vec::<&&String>::new(),
        "{what}: files missing from export"
    );
    assert_eq!(
        gk.difference(&wk).collect::<Vec<_>>(),
        Vec::<&&String>::new(),
        "{what}: unexpected files in export"
    );
    for (k, v) in &want.0 {
        assert!(
            got.0[k] == *v,
            "{what}: {k} differs ({} vs {} bytes)",
            v.len(),
            got.0[k].len()
        );
    }
    assert_eq!(want.1, got.1, "{what}: folders differ");
}

struct EngineExisting<'a>(&'a mut Engine);
impl Existing for EngineExisting<'_> {
    fn state(&self) -> &MetaState {
        &self.0.state
    }
    fn text(&mut self, id: Id) -> Option<Vec<u8>> {
        self.0.doc_text(id, "body").ok().map(String::into_bytes)
    }
}

fn engine() -> Engine {
    Engine::open(db::open_memory().unwrap(), EngineConfig::default(), 3).unwrap()
}

fn export_both(e: &Engine, fs: &BlobFs) -> (Tree, Tree) {
    let mut snap = Snapshot::begin(&e.conn, fs).unwrap();
    let p = project(&snap.state, Options::default());
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    let out = tempfile::tempdir().unwrap();
    write_folder(&p, &mut snap, out.path()).unwrap();
    let z = write_zip(&p, &mut snap, Vec::new(), |_, _| {}).unwrap();
    (read_tree(out.path()), zip_tree(&z))
}

fn server_import(e: &mut Engine, fs: &BlobFs, src: &mut dyn import::ImportSource) -> import::Plan {
    let plan = import::plan(src, &mut EngineExisting(e), &PlanOptions::default()).unwrap();
    let st = e.state.clone();
    let mut sink = ServerSink::new(&mut *e, fs, 1_700_000_000_000, 9);
    import::execute(&plan, src, &st, &mut sink, |_, _| {}).unwrap();
    assert!(sink.rejected.is_empty(), "{:?}", sink.rejected);
    plan
}

#[test]
fn server_path_folder_roundtrip_and_idempotency() {
    let vault = prepare();
    let want = read_tree(vault.path());
    let mut e = engine();
    let bd = tempfile::tempdir().unwrap();
    let fs = BlobFs::new(bd.path(), false).unwrap();
    let plan = server_import(
        &mut e,
        &fs,
        &mut FolderSource {
            root: vault.path().into(),
        },
    );
    let r = &plan.report;
    assert_eq!(r.pdfs, 3);
    assert_eq!(
        r.pdfs_hidden, 1,
        "attachments/manual.pdf is hidden by the attachment-folder rule"
    );
    assert!(
        r.skipped.iter().any(|(p, _)| p == ".obsidian/"),
        "{:?}",
        r.skipped
    );
    assert!(r.skipped.iter().any(|(p, _)| p == ".trash/"));
    assert!(r.skipped.iter().any(|(p, _)| p.ends_with(".DS_Store")));
    assert!(
        r.unresolved.iter().any(|(_, t)| t == "[[Does not exist]]"),
        "{:?}",
        r.unresolved
    );
    assert!(r.unresolved.iter().any(|(_, t)| t == "[[missing.png]]"));
    assert!(
        !r.unresolved
            .iter()
            .any(|(_, t)| t.contains("shared.png") || t.contains("per-note")),
        "{:?}",
        r.unresolved
    );
    assert!(r.collisions.iter().any(|c| c == "Case/A.md / Case/a.md"));
    assert_eq!(
        plan.settings.attachment_folder_path.as_deref(),
        Some("attachments")
    );
    // Kinds and visibility.
    let by_path = |p: &str| e.state.get(&e.state.by_path(p).unwrap()).unwrap().clone();
    assert!(!by_path("attachments/manual.pdf").tree_visible);
    assert!(by_path("Docs/Paper.pdf").tree_visible);
    assert!(!by_path("attachments/shared.png").tree_visible);
    assert_eq!(by_path("Latin1.md").kind, "markdown");
    assert!(
        by_path("Latin1.md").blob.is_some(),
        "non-UTF-8 note is blob-backed"
    );
    let jpg = by_path("Notes/same-folder.jpg");
    let row: (i64, i64, i64) = e
        .conn
        .query_row(
            "SELECT width, height, orientation FROM blobs WHERE hash = ?1",
            [jpg.blob.unwrap().0.to_vec()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap_or((0, 0, 0));
    let _ = row;
    let (folder, zipped) = export_both(&e, &fs);
    assert_same(&want, &folder, "folder export");
    assert_same(&want, &zipped, "zip export");
    // Vault settings adopted from .obsidian/app.json.
    let v = e.state.get(&jess_core::ids::VAULT_SETTINGS_ID).unwrap();
    assert_eq!(
        v.props["attachmentFolderPath"].as_str().as_deref(),
        Some("attachments")
    );
    // Idempotent re-import: nothing new, nothing changed.
    let head = e.head;
    let plan2 = import::plan(
        &mut FolderSource {
            root: vault.path().into(),
        },
        &mut EngineExisting(&mut e),
        &PlanOptions::default(),
    )
    .unwrap();
    assert_eq!(
        plan2.report.notes + plan2.report.pdfs + plan2.report.images + plan2.report.other_media,
        0,
        "{:?}",
        plan2.report
    );
    assert_eq!(plan2.report.conflicts, 0);
    let st = e.state.clone();
    import::execute(
        &plan2,
        &mut FolderSource {
            root: vault.path().into(),
        },
        &st,
        &mut ServerSink::new(&mut e, &fs, 1, 1),
        |_, _| {},
    )
    .unwrap();
    // Only the (unchanged) vault settings may be rewritten; no entries or docs added.
    let docs: i64 = e
        .conn
        .query_row(
            "SELECT count(*) FROM doc_updates WHERE seq > ?1",
            [head as i64],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(docs, 0);
    // A changed file is a conflict (ask by default), overwrite applies a minimal diff.
    std::fs::write(vault.path().join("Welcome.md"), "changed\n").unwrap();
    let mut p3 = import::plan(
        &mut FolderSource {
            root: vault.path().into(),
        },
        &mut EngineExisting(&mut e),
        &PlanOptions::default(),
    )
    .unwrap();
    assert_eq!(p3.report.conflicts, 1);
    assert!(
        import::execute(
            &p3,
            &mut FolderSource {
                root: vault.path().into()
            },
            &st,
            &mut ServerSink::new(&mut e, &fs, 1, 1),
            |_, _| {}
        )
        .is_err(),
        "ask must be resolved"
    );
    for it in p3.items.iter_mut() {
        if let import::Action::Conflict { resolution, .. } = &mut it.action {
            *resolution = import::Conflict::Overwrite;
        }
    }
    let st = e.state.clone();
    import::execute(
        &p3,
        &mut FolderSource {
            root: vault.path().into(),
        },
        &st,
        &mut ServerSink::new(&mut e, &fs, 1, 1),
        |_, _| {},
    )
    .unwrap();
    let id = e.state.by_path("Welcome.md").unwrap();
    assert_eq!(e.doc_text(id, "body").unwrap(), "changed\n");
}

#[test]
fn server_path_zip_roundtrip_matches_folder_path() {
    let vault = prepare();
    let want = read_tree(vault.path());
    // Zip the source vault inside a top-level folder, as vault zips usually are.
    let mut z = jess_core::zipstream::ZipStream::new(Vec::new());
    let mut stack = vec![(vault.path().to_path_buf(), String::new())];
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
                z.add_dir(&format!("My Vault/{r}"), None).unwrap();
                stack.push((e.path(), r));
            } else {
                let b = std::fs::read(e.path()).unwrap();
                z.add_bytes(&format!("My Vault/{r}"), &b, None, true)
                    .unwrap();
            }
        }
    }
    let zbytes = z.finish().unwrap();
    let mut e = engine();
    let bd = tempfile::tempdir().unwrap();
    let fs = BlobFs::new(bd.path(), false).unwrap();
    let mut src = ZipSource::new(std::io::Cursor::new(zbytes)).unwrap();
    server_import(&mut e, &fs, &mut src);
    let (folder, zipped) = export_both(&e, &fs);
    assert_same(&want, &folder, "zip import → folder export");
    assert_same(&want, &zipped, "zip import → zip export");
}

#[test]
fn client_path_roundtrip() {
    let vault = prepare();
    let want = read_tree(vault.path());
    let mut e = engine();
    let bd = tempfile::tempdir().unwrap();
    let fs = BlobFs::new(bd.path(), false).unwrap();
    let mut client = InProc::new(501);
    client.connect(&e);
    client.sync(&mut e, &fs);
    let mut src = FolderSource {
        root: vault.path().into(),
    };
    struct ClientExisting<'a>(&'a MetaState);
    impl Existing for ClientExisting<'_> {
        fn state(&self) -> &MetaState {
            self.0
        }
        fn text(&mut self, _: Id) -> Option<Vec<u8>> {
            None
        }
    }
    let view = client.c.view().clone();
    let plan = import::plan(
        &mut src,
        &mut ClientExisting(&view),
        &PlanOptions::default(),
    )
    .unwrap();
    import::execute(
        &plan,
        &mut src,
        &view,
        &mut ClientSink {
            p: &mut client,
            n: 0,
        },
        |_, _| {},
    )
    .unwrap();
    client.sync(&mut e, &fs);
    assert!(
        client.c.quarantine().next().is_none(),
        "{:?}",
        client.c.quarantine().next()
    );
    assert_eq!(client.c.pending_count(), 0);
    let (folder, zipped) = export_both(&e, &fs);
    assert_same(&want, &folder, "client import → server export");
    assert_same(&want, &zipped, "client import → server zip");
}

#[test]
fn portable_export_reports_mappings() {
    let vault = prepare();
    let mut e = engine();
    let bd = tempfile::tempdir().unwrap();
    let fs = BlobFs::new(bd.path(), false).unwrap();
    server_import(
        &mut e,
        &fs,
        &mut FolderSource {
            root: vault.path().into(),
        },
    );
    let mut snap = Snapshot::begin(&e.conn, &fs).unwrap();
    let p = project(
        &snap.state,
        Options {
            profile: Profile::Portable,
            include_trash: false,
        },
    );
    let z = write_zip(&p, &mut snap, Vec::new(), |_, _| {}).unwrap();
    let t = zip_tree(&z);
    let report = String::from_utf8(t.0["EXPORT-REPORT.txt"].clone()).unwrap();
    assert!(
        report.contains("Case/A.md  ->  Case/A (2).md")
            || report.contains("Case/a.md  ->  Case/a (2).md"),
        "{report}"
    );
    assert!(report.contains("Special/Name with #hash? and [brackets].md  ->  Special/Name with #hash_ and [brackets].md"), "{report}");
    assert!(
        report.contains("Trailing space   ->  Trailing space"),
        "{report}"
    );
    assert!(t.0.keys().all(|k| !k.contains('?')));
}
