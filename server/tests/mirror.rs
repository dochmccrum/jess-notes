//! Mirror and git (DESIGN §13, §17.8).

use jess_core::import::{self, Existing, FolderSource, PlanOptions};
use jess_core::model::KIND_MARKDOWN;
use jess_core::ops::{MetaOp, Op, OpBody};
use jess_core::state::MetaState;
use jess_core::Id;
use jess_server::blobfs::BlobFs;
use jess_server::db;
use jess_server::engine::{Engine, EngineConfig};
use jess_server::git::{commit_message, Git, GitConfig};
use jess_server::mirror::{check_with, Mirror};
use jess_server::vault_io::ServerSink;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

struct Ex<'a>(&'a mut Engine);
impl Existing for Ex<'_> {
    fn state(&self) -> &MetaState {
        &self.0.state
    }
    fn text(&mut self, id: Id) -> Option<Vec<u8>> {
        self.0.doc_text(id, "body").ok().map(String::into_bytes)
    }
}

fn setup() -> (tempfile::TempDir, Engine, BlobFs) {
    let d = tempfile::tempdir().unwrap();
    let conn = db::open(&d.path().join("jess.db"), false).unwrap();
    let e = Engine::open(conn, EngineConfig::default(), 5).unwrap();
    let fs = BlobFs::new(d.path().join("blobs"), false).unwrap();
    (d, e, fs)
}

fn import_fixture(e: &mut Engine, fs: &BlobFs) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/vault");
    let mut src = FolderSource { root };
    let plan = import::plan(&mut src, &mut Ex(e), &PlanOptions::default()).unwrap();
    let st = e.state.clone();
    import::execute(
        &plan,
        &mut src,
        &st,
        &mut ServerSink::new(&mut *e, fs, 1_700_000_000_000, 1),
        |_, _| {},
    )
    .unwrap();
}

fn git(repo: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn op(e: &Engine, body: MetaOp) -> Op {
    Op {
        op_id: e.head + 1000,
        hlc: jess_core::hlc::Hlc::new(1_900_000_000_000 + e.head, 0, 42),
        known_seq: e.head,
        group: 0,
        body: OpBody::Meta(body),
    }
}

#[test]
fn mirror_equals_projection_hardlinks_and_git() {
    let (d, mut e, fs) = setup();
    import_fixture(&mut e, &fs);
    let root = d.path().join("mirror");
    let mut m = Mirror::open(
        root.clone(),
        &d.path().join("mirror-state.db"),
        d.path().join("jess.db"),
        fs.clone(),
        false,
    )
    .unwrap();
    let ch = m.sync_with(&e.conn).unwrap();
    assert!(ch.added.len() > 70, "{}", ch.added.len());
    assert_eq!(
        check_with(&root, &e.conn, &fs).unwrap(),
        Vec::<String>::new()
    );
    assert!(root.join("README-GENERATED.md").exists() && root.join(".jess-generated").exists());
    // Blobs are hardlinks into the blob store (no extra disk).
    let png = root.join("attachments/shared.png");
    let id = e.state.by_path("attachments/shared.png").unwrap();
    let h = e.state.get(&id).unwrap().blob.unwrap();
    assert_eq!(
        std::fs::metadata(&png).unwrap().ino(),
        std::fs::metadata(fs.path(&h)).unwrap().ino()
    );
    // Same bytes as the source vault.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/vault");
    for p in [
        "Welcome.md",
        "Windows note.md",
        "Latin1.md",
        "Unicode/Cafe\u{301} NFD.md",
        "Notes/My Note.assets/per-note.png",
    ] {
        assert_eq!(
            std::fs::read(root.join(p)).unwrap(),
            std::fs::read(src.join(p)).unwrap(),
            "{p}"
        );
    }
    // A second sync with nothing new does nothing.
    assert!(m.sync_with(&e.conn).unwrap().is_empty());

    if !Git::available() {
        eprintln!("git not installed: skipping git assertions");
        return;
    }
    let g = Git {
        cfg: GitConfig {
            repo: root.clone(),
            key_dir: d.path().join("git"),
            remote: None,
            include_attachments: false,
            lfs: false,
        },
    };
    g.init().unwrap();
    g.commit(&commit_message(&ch)).unwrap().unwrap();
    let files = git(&root, &["ls-files"]);
    assert!(
        files
            .lines()
            .all(|l| l.to_lowercase().ends_with(".md") || l.starts_with('"')),
        "attachments must be excluded: {files}"
    );
    assert!(files.contains("Welcome.md") && !files.contains("shared.png"));
    assert!(git(&root, &["log", "-1", "--format=%s"]).starts_with("Jess: "));

    // Rename → one rename in the next commit; the old file is gone.
    let plan = e.state.by_path("Projects/Plan.md").unwrap();
    let o = op(
        &e,
        MetaOp::SetName {
            id: plan,
            name: "Roadmap.md".into(),
        },
    );
    e.push(42, None, &[o], 2_000_000_000_000).unwrap();
    let ch = m.sync_with(&e.conn).unwrap();
    assert_eq!(
        ch.renamed,
        vec![(
            "Projects/Plan.md".to_string(),
            "Projects/Roadmap.md".to_string()
        )]
    );
    assert!(!root.join("Projects/Plan.md").exists() && root.join("Projects/Roadmap.md").exists());
    g.commit(&commit_message(&ch)).unwrap();
    // The rename also rewrote `[[Projects/Plan]]` in Welcome.md (Invariant R).
    assert_eq!(
        git(&root, &["log", "-1", "--format=%s"]).trim(),
        "Jess: 1 edited, 1 renamed (Plan → Roadmap)"
    );
    assert!(std::fs::read_to_string(root.join("Welcome.md"))
        .unwrap()
        .contains("[[Projects/Roadmap]]"));
    // Collision: a second Welcome.md gets a suffix.
    let nid = Id([77; 16]);
    let o = op(
        &e,
        MetaOp::Create {
            id: nid,
            kind: KIND_MARKDOWN.into(),
            parent: None,
            name: "Welcome.md".into(),
            tree_visible: true,
            blob: None,
            blob_info: None,
            created_at: None,
            modified_at: None,
            props: vec![],
        },
    );
    e.push(42, None, &[o], 2_000_000_000_000).unwrap();
    let ch = m.sync_with(&e.conn).unwrap();
    assert!(ch.added.contains(&"Welcome 1.md".to_string()), "{ch:?}");
    // Trash a folder → its files disappear; empty directories are pruned.
    let many = e.state.by_path("Many").unwrap();
    let o = op(&e, MetaOp::Trash { id: many });
    e.push(42, None, &[o], 2_000_000_000_000).unwrap();
    let ch = m.sync_with(&e.conn).unwrap();
    assert_eq!(ch.deleted.len(), 50);
    assert!(!root.join("Many").exists());
    g.commit(&commit_message(&ch)).unwrap();
    assert!(git(&root, &["log", "-1", "--format=%s"]).contains("50 deleted"));
    assert_eq!(
        check_with(&root, &e.conn, &fs).unwrap(),
        Vec::<String>::new()
    );
    // A stray temp file (simulated crash mid-write) is cleaned on the next open.
    std::fs::write(root.join("Projects/.jess-tmp-deadbeef"), b"partial").unwrap();
    std::fs::write(root.join("Welcome.md"), b"tampered").unwrap();
    drop(m);
    let mut m = Mirror::open(
        root.clone(),
        &d.path().join("mirror-state.db"),
        d.path().join("jess.db"),
        fs.clone(),
        false,
    )
    .unwrap();
    e.push(
        42,
        None,
        &[op(
            &e,
            MetaOp::SetVisible {
                id: nid,
                visible: true,
            },
        )],
        2_000_000_000_000,
    )
    .unwrap();
    m.sync_with(&e.conn).unwrap();
    assert_eq!(
        check_with(&root, &e.conn, &fs).unwrap(),
        Vec::<String>::new(),
        "tampered file restored, temp removed"
    );
    // rebuild keeps .git and regenerates everything.
    m.rebuild().unwrap();
    assert!(root.join(".git").exists());
    assert_eq!(
        check_with(&root, &e.conn, &fs).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn blocked_mirror_does_not_slow_sync() {
    // A mirror stuck mid-read holds a WAL read transaction; the writer must not care.
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("jess.db");
    let conn = db::open(&path, true).unwrap();
    let mut e = Engine::open(conn, EngineConfig::default(), 5).unwrap();
    let reader = db::open_readonly(&path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let _: i64 = reader
        .query_row("SELECT count(*) FROM entries", [], |r| r.get(0))
        .unwrap();
    let id = Id([5; 16]);
    let mut lat = Vec::new();
    for i in 0..200u64 {
        let body = if i == 0 {
            OpBody::Meta(MetaOp::Create {
                id,
                kind: KIND_MARKDOWN.into(),
                parent: None,
                name: "n.md".into(),
                tree_visible: true,
                blob: None,
                blob_info: None,
                created_at: None,
                modified_at: None,
                props: vec![],
            })
        } else {
            let d = jess_core::doc::new_doc(i + 10);
            OpBody::Doc {
                entry: id,
                slot: "body".into(),
                update: jess_core::doc::insert(&d, 0, "x"),
            }
        };
        let o = Op {
            op_id: i + 1,
            hlc: jess_core::hlc::Hlc::new(1000 + i, 0, 9),
            known_seq: e.head,
            group: 0,
            body,
        };
        let t = Instant::now();
        e.push(9, None, &[o], 0).unwrap();
        lat.push(t.elapsed());
    }
    lat.sort();
    let p99 = lat[lat.len() * 99 / 100];
    eprintln!(
        "push p50 {:?} p99 {:?} with a blocked reader",
        lat[lat.len() / 2],
        p99
    );
    assert!(p99 < Duration::from_millis(100), "p99 {p99:?}");
    reader.execute_batch("COMMIT").unwrap();
}
