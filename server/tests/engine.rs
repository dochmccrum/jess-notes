//! Deterministic engine scenarios for DESIGN §6 (conflict semantics).

use jess_core::doc as ydoc;
use jess_core::hlc::Hlc;
use jess_core::model::{KIND_FOLDER, KIND_MARKDOWN};
use jess_core::ops::{AckResult, MetaOp, Op, OpBody, Reject};
use jess_core::Id;
use jess_server::db;
use jess_server::engine::{Engine, EngineConfig};

struct T {
    e: Engine,
    op: u64,
}

fn id(n: u8) -> Id {
    Id([n; 16])
}

impl T {
    fn new() -> T {
        T {
            e: Engine::open(db::open_memory().unwrap(), EngineConfig::default(), 7).unwrap(),
            op: 0,
        }
    }
    fn push(&mut self, replica: u64, known: u64, bodies: Vec<OpBody>) -> Vec<AckResult> {
        let ops: Vec<Op> = bodies
            .into_iter()
            .map(|body| {
                self.op += 1;
                Op {
                    op_id: self.op,
                    hlc: Hlc::new(1000 + self.op, 0, replica),
                    known_seq: known,
                    group: 0,
                    body,
                }
            })
            .collect();
        self.e
            .push(replica, None, &ops, 2_000_000)
            .unwrap()
            .into_iter()
            .map(|a| a.1)
            .collect()
    }
    fn create(&mut self, i: u8, parent: Option<u8>, name: &str, folder: bool) {
        let head = self.e.head;
        let r = self.push(
            1,
            head,
            vec![OpBody::Meta(MetaOp::Create {
                id: id(i),
                kind: if folder { KIND_FOLDER } else { KIND_MARKDOWN }.into(),
                parent: parent.map(id),
                name: name.into(),
                tree_visible: true,
                blob: None,
                blob_info: None,
                created_at: None,
                modified_at: None,
                props: vec![],
            })],
        );
        assert!(matches!(r[0], AckResult::Applied(_)));
    }
    fn write(&mut self, i: u8, text: &str, known: u64, replica: u64) {
        let d = ydoc::new_doc(1000 + self.op);
        for r in self.e.doc_rows(id(i), "body").unwrap() {
            ydoc::apply(&d, &r).unwrap();
        }
        let u = ydoc::set_text(&d, text);
        self.push(
            replica,
            known,
            vec![OpBody::Doc {
                entry: id(i),
                slot: "body".into(),
                update: u,
            }],
        );
    }
    fn text(&mut self, i: u8) -> String {
        self.e.doc_text(id(i), "body").unwrap()
    }
    fn rename(&mut self, i: u8, name: &str) {
        let h = self.e.head;
        self.push(
            2,
            h,
            vec![OpBody::Meta(MetaOp::SetName {
                id: id(i),
                name: name.into(),
            })],
        );
    }
    fn mv(&mut self, i: u8, p: Option<u8>) -> AckResult {
        let h = self.e.head;
        self.push(
            2,
            h,
            vec![OpBody::Meta(MetaOp::SetParent {
                id: id(i),
                parent: p.map(id),
            })],
        )[0]
    }
}

#[test]
fn rename_rewrites_all_styles_preserving_suffixes() {
    let mut t = T::new();
    t.create(1, None, "Old.md", false);
    t.create(2, None, "L.md", false);
    let h = t.e.head;
    t.write(2, "[[Old]] [[Old|alias]] [[Old#Heading]] ![[Old#^blk]] [t](Old.md) [t](<Old.md>) [[old.md]] `[[Old]]`", h, 1);
    t.rename(1, "New name.md");
    assert_eq!(t.text(2), "[[New name]] [[New name|alias]] [[New name#Heading]] ![[New name#^blk]] [t](New%20name.md) [t](<New name.md>) [[New name.md]] `[[Old]]`");
}

#[test]
fn pdf_rename_keeps_page_and_height() {
    let mut t = T::new();
    let h = t.e.head;
    t.push(
        1,
        h,
        vec![OpBody::Meta(MetaOp::Create {
            id: id(1),
            kind: "pdf".into(),
            parent: None,
            name: "scan.PDF".into(),
            tree_visible: true,
            blob: None,
            blob_info: None,
            created_at: None,
            modified_at: None,
            props: vec![],
        })],
    );
    t.create(2, None, "L.md", false);
    let h = t.e.head;
    t.write(2, "![[scan.PDF#page=3&height=600]] [[scan.PDF]]", h, 1);
    t.rename(1, "Paper.pdf");
    assert_eq!(t.text(2), "![[Paper.pdf#page=3&height=600]] [[Paper.pdf]]");
}

#[test]
fn moving_a_note_rewrites_its_own_relative_links_and_inbound_paths() {
    let mut t = T::new();
    t.create(10, None, "A", true);
    t.create(11, None, "B", true);
    t.create(12, Some(11), "C", true);
    t.create(1, Some(10), "Mover.md", false);
    t.create(3, None, "In.md", false);
    t.create(4, None, "Twin.md", false);
    t.create(5, Some(10), "Twin.md", false);
    let h = t.e.head;
    // `./Twin` means A/Twin; `../In` means In.md. Both would change meaning after the move.
    t.write(1, "[[./Twin]] [[../In]] [t](Twin.md)", h, 1);
    t.write(3, "[[A/Mover]]", t.e.head, 1);
    t.mv(1, Some(12));
    assert_eq!(
        t.text(1),
        "[[../../A/Twin]] [[../../In]] [t](../../A/Twin.md)"
    );
    // `A/Mover` would otherwise fall through to nothing: rewritten to the new path.
    assert_eq!(t.text(3), "[[B/C/Mover]]");
}

#[test]
fn folder_move_keeps_suffix_links_and_rewrites_absolute_ones() {
    let mut t = T::new();
    t.create(10, None, "Proj", true);
    t.create(11, None, "Archive", true);
    t.create(1, Some(10), "Plan.md", false);
    t.create(2, None, "Index.md", false);
    t.create(3, None, "Plan.md", false);
    let h = t.e.head;
    // [[Proj/Plan]] still resolves by suffix after the move (no rewrite needed); the markdown
    // link resolves vault-absolute, which would fall back to the root Plan.md, so it is rewritten.
    t.write(2, "[[Proj/Plan]] [p](/Proj/Plan.md)", h, 1);
    t.mv(10, Some(11));
    assert_eq!(t.text(2), "[[Proj/Plan]] [p](/Archive/Proj/Plan.md)");
    let r =
        t.e.ix
            .resolve("Proj/Plan", jess_core::links::Syntax::Wiki, "")
            .unwrap()
            .id;
    assert_eq!(r, id(1));
}

#[test]
fn move_creating_ambiguity_rewrites_bare_link_to_path() {
    let mut t = T::new();
    t.create(10, None, "Deep", true);
    t.create(11, None, "Other", true);
    t.create(1, Some(10), "Name.md", false);
    t.create(2, Some(11), "Name.md", false);
    t.create(3, None, "Linker.md", false);
    // Only Deep/Name.md and Other/Name.md exist: [[Name]] resolves by ordering to Deep/Name.
    let h = t.e.head;
    t.write(3, "[[Name]]", h, 1);
    let before =
        t.e.ix
            .resolve("Name", jess_core::links::Syntax::Wiki, "")
            .unwrap()
            .id;
    // Moving the other one to the root would steal the bare link: it gets rewritten to a path.
    let mover = if before == id(1) { 2 } else { 1 };
    t.mv(mover, None);
    let target_path = t.e.state.path_of(before).unwrap();
    assert_eq!(
        t.text(3),
        format!("[[{}]]", target_path.trim_end_matches(".md"))
    );
}

#[test]
fn stale_link_from_offline_replica_follows_rename() {
    let mut t = T::new();
    t.create(1, None, "Old.md", false);
    t.create(2, None, "L.md", false);
    let before_rename = t.e.head;
    t.rename(1, "New.md");
    // Replica 9 never saw the rename: it adds [[Old]] with known_seq before it.
    t.write(2, "offline [[Old]]", before_rename, 9);
    assert_eq!(t.text(2), "offline [[New]]");
    // A link to a name that was never renamed stays unresolved and untouched.
    let h = t.e.head;
    t.write(2, "offline [[New]] [[Nope]]", h, 9);
    assert_eq!(t.text(2), "offline [[New]] [[Nope]]");
}

#[test]
fn stale_link_follows_collision_suffix() {
    let mut t = T::new();
    t.create(1, None, "Untitled.md", false);
    let h0 = t.e.head;
    // Replica 5 creates its own Untitled.md offline and links to it.
    t.push(
        5,
        h0 - 1,
        vec![OpBody::Meta(MetaOp::Create {
            id: id(2),
            kind: KIND_MARKDOWN.into(),
            parent: None,
            name: "Untitled.md".into(),
            tree_visible: true,
            blob: None,
            blob_info: None,
            created_at: None,
            modified_at: None,
            props: vec![],
        })],
    );
    assert_eq!(t.e.state.get(&id(2)).unwrap().name, "Untitled 1.md");
    t.create(3, None, "L.md", false);
    t.write(3, "[[Untitled]]", h0 - 1, 5);
    // Ambiguous by design (both replicas' notes are plausible): only if exactly one rename
    // explains it is it rewritten. Here the induced rename of id(2) explains it.
    assert_eq!(t.text(3), "[[Untitled 1]]");
}

#[test]
fn cycle_and_trash_semantics() {
    let mut t = T::new();
    t.create(10, None, "A", true);
    t.create(11, Some(10), "B", true);
    assert_eq!(t.mv(10, Some(11)), AckResult::Rejected(Reject::Cycle));
    t.create(1, Some(11), "n.md", false);
    let h = t.e.head;
    t.push(1, h, vec![OpBody::Meta(MetaOp::Trash { id: id(10) })]);
    assert!(t.e.state.get(&id(1)).unwrap().trashed.is_some());
    // Edit to a trashed note is applied (delete wins location, edit wins content).
    t.write(1, "still editing", h, 3);
    assert_eq!(t.text(1), "still editing");
    assert!(t.e.state.get(&id(1)).unwrap().trashed.is_some());
    // Purge, then an offline edit with new content recovers it into trash.
    let h = t.e.head;
    t.push(1, h, vec![OpBody::Meta(MetaOp::Purge { id: id(10) })]);
    assert!(t.e.state.get(&id(1)).unwrap().purged);
    let d = ydoc::new_doc(4242);
    let u = ydoc::insert(&d, 0, "offline words");
    let r = t.push(
        7,
        1,
        vec![OpBody::Doc {
            entry: id(1),
            slot: "body".into(),
            update: u.clone(),
        }],
    );
    assert!(matches!(r[0], AckResult::Applied(_)));
    let e = t.e.state.get(&id(1)).unwrap().clone();
    assert!(!e.purged && e.trashed.is_some());
    assert_eq!(e.name, "Recovered — n.md");
    assert!(t.text(1).contains("offline words") && t.text(1).contains("still editing"));
    // Replaying already-covered content after another purge does not resurrect it.
    let h = t.e.head;
    t.push(1, h, vec![OpBody::Meta(MetaOp::Purge { id: id(1) })]);
    let r = t.push(
        8,
        1,
        vec![OpBody::Doc {
            entry: id(1),
            slot: "body".into(),
            update: u,
        }],
    );
    assert_eq!(r[0], AckResult::Duplicate(0));
    assert!(t.e.state.get(&id(1)).unwrap().purged);
}

#[test]
fn reordered_and_duplicated_pushes_are_idempotent() {
    let mut t = T::new();
    t.create(1, None, "n.md", false);
    let d = ydoc::new_doc(5);
    let u1 = ydoc::insert(&d, 0, "a");
    let u2 = ydoc::insert(&d, 1, "b");
    let op = |op_id, u: Vec<u8>| Op {
        op_id,
        hlc: Hlc::new(5000 + op_id, 0, 9),
        known_seq: 0,
        group: 0,
        body: OpBody::Doc {
            entry: id(1),
            slot: "body".into(),
            update: u,
        },
    };
    let r2 = t.e.push(9, None, &[op(2, u2.clone())], 0).unwrap();
    let r1 = t.e.push(9, None, &[op(1, u1.clone())], 0).unwrap();
    assert!(
        matches!(r2[0].1, AckResult::Applied(_)) && matches!(r1[0].1, AckResult::Applied(_)),
        "reordered op 1 must still apply"
    );
    let again = t.e.push(9, None, &[op(1, u1), op(2, u2)], 0).unwrap();
    assert!(again
        .iter()
        .all(|a| matches!(a.1, AckResult::Duplicate(s) if s > 0)));
    assert_eq!(t.text(1), "ab");
}

#[test]
fn group_is_atomic() {
    let mut t = T::new();
    t.create(10, None, "A", true);
    t.create(1, None, "n.md", false);
    let h = t.e.head;
    let ops = vec![
        Op {
            op_id: 100,
            hlc: Hlc::new(9000, 0, 4),
            known_seq: h,
            group: 100,
            body: OpBody::Meta(MetaOp::SetName {
                id: id(1),
                name: "m.md".into(),
            }),
        },
        Op {
            op_id: 101,
            hlc: Hlc::new(9001, 0, 4),
            known_seq: h,
            group: 100,
            body: OpBody::Meta(MetaOp::SetParent {
                id: id(10),
                parent: Some(id(10)),
            }),
        },
    ];
    let r = t.e.push(4, None, &ops, 0).unwrap();
    assert_eq!(r[0].1, AckResult::Rejected(Reject::GroupFailed));
    assert_eq!(r[1].1, AckResult::Rejected(Reject::Cycle));
    assert_eq!(t.e.state.get(&id(1)).unwrap().name, "n.md");
}
