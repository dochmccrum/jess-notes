//! Property tests through the real engine (DESIGN §17.2): rename/move races with link rewriting
//! (notes and PDFs), stale links from replicas that hadn't seen the rename, and edits inside a
//! link racing its rewrite (never loses characters).

use jess_core::doc as ydoc;
use jess_core::hlc::Hlc;
use jess_core::links::extract;
use jess_core::model::{KIND_FOLDER, KIND_MARKDOWN, KIND_PDF};
use jess_core::ops::{MetaOp, Op, OpBody};
use jess_core::Id;
use jess_server::db;
use jess_server::engine::{Engine, EngineConfig};
use proptest::prelude::*;

const NAMES: &[&str] = &[
    "Alpha.md", "alpha.md", "Beta.md", "doc.pdf", "Doc.PDF", "x y.md",
];

struct V {
    e: Engine,
    folders: Vec<Id>,
    targets: Vec<Id>,
    linker: Id,
    doc: yrs::Doc,
    op: u64,
}

impl V {
    fn new() -> V {
        let e = Engine::open(db::open_memory().unwrap(), EngineConfig::default(), 1).unwrap();
        let mut v = V {
            e,
            folders: vec![],
            targets: vec![],
            linker: Id([200; 16]),
            doc: ydoc::new_doc(99),
            op: 0,
        };
        let mut ops = vec![];
        for i in 0..3u8 {
            let id = Id([100 + i; 16]);
            v.folders.push(id);
            ops.push(MetaOp::Create {
                id,
                kind: KIND_FOLDER.into(),
                parent: if i == 2 { Some(Id([100; 16])) } else { None },
                name: ["F", "G", "Sub"][i as usize].into(),
                tree_visible: true,
                blob: None,
                blob_info: None,
                created_at: None,
                modified_at: None,
                props: vec![],
            });
        }
        for i in 0..4u8 {
            let id = Id([i + 1; 16]);
            v.targets.push(id);
            let kind = if i == 3 { KIND_PDF } else { KIND_MARKDOWN };
            let name = ["Alpha.md", "Beta.md", "Gamma.md", "file.pdf"][i as usize];
            ops.push(MetaOp::Create {
                id,
                kind: kind.into(),
                parent: Some(v.folders[(i % 3) as usize]),
                name: name.into(),
                tree_visible: true,
                blob: None,
                blob_info: None,
                created_at: None,
                modified_at: None,
                props: vec![],
            });
        }
        ops.push(MetaOp::Create {
            id: v.linker,
            kind: KIND_MARKDOWN.into(),
            parent: None,
            name: "Linker.md".into(),
            tree_visible: true,
            blob: None,
            blob_info: None,
            created_at: None,
            modified_at: None,
            props: vec![],
        });
        v.push(1, 0, ops.into_iter().map(OpBody::Meta).collect());
        let text = "[[Alpha]] [[F/Alpha|a]] ![[Beta#h]] [g](G/Beta.md) [s](<F/Sub/Gamma.md>) ![[file.pdf#page=3&height=600]] [p](F/file.pdf)\n";
        let u = ydoc::insert(&v.doc, 0, text);
        v.push(
            1,
            0,
            vec![OpBody::Doc {
                entry: v.linker,
                slot: "body".into(),
                update: u,
            }],
        );
        v
    }
    fn push(&mut self, replica: u64, known_seq: u64, bodies: Vec<OpBody>) {
        let ops: Vec<Op> = bodies
            .into_iter()
            .map(|body| {
                self.op += 1;
                Op {
                    op_id: self.op,
                    hlc: Hlc::new(1000 + self.op, 0, replica),
                    known_seq,
                    group: 0,
                    body,
                }
            })
            .collect();
        self.e
            .push(replica, None, &ops, 1_000_000 + self.op)
            .unwrap();
    }
    fn resolutions(&mut self) -> Vec<Option<Id>> {
        let text = self.e.doc_text(self.linker, "body").unwrap();
        let folder = self.e.state.folder_of(self.linker);
        extract(&text)
            .links
            .iter()
            .map(|l| {
                self.e
                    .ix
                    .resolve(&l.target, l.syntax, &folder)
                    .map(|r| r.id)
            })
            .collect()
    }
    fn sync_doc(&mut self) {
        let st = self.e.doc_state(self.linker, "body").unwrap();
        ydoc::apply(&self.doc, &st).unwrap();
    }
}

#[derive(Debug, Clone)]
enum Act {
    Rename {
        t: usize,
        name: usize,
        stale: bool,
    },
    Move {
        t: usize,
        f: Option<usize>,
        stale: bool,
    },
    MoveFolder {
        f: usize,
        into_root: bool,
    },
    AppendStaleLink {
        t_name: usize,
    },
    EditInsideLink {
        at: usize,
        s: String,
    },
}

fn act() -> impl Strategy<Value = Act> {
    prop_oneof![
        (0..4usize, 0..NAMES.len(), any::<bool>()).prop_map(|(t, name, stale)| Act::Rename {
            t,
            name,
            stale
        }),
        (0..4usize, prop::option::of(0..3usize), any::<bool>())
            .prop_map(|(t, f, stale)| Act::Move { t, f, stale }),
        (0..3usize, any::<bool>()).prop_map(|(f, into_root)| Act::MoveFolder { f, into_root }),
        (0..4usize).prop_map(|t_name| Act::AppendStaleLink { t_name }),
        (0..60usize, "[a-z]{1,3}").prop_map(|(at, s)| Act::EditInsideLink { at, s }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(120))]

    #[test]
    fn renames_keep_links_and_edits_survive(acts in prop::collection::vec(act(), 1..14)) {
        let mut v = V::new();
        let base_seq = v.e.head;
        let original = v.resolutions();
        prop_assert!(original.iter().all(|r| r.is_some()), "fixture links resolve: {:?}", original);
        let mut inserted = String::new();
        let mut edited = false;
        for a in acts {
            let known = if matches!(a, Act::Rename { stale: true, .. } | Act::Move { stale: true, .. }) { base_seq } else { v.e.head };
            match a {
                Act::Rename { t, name, .. } => {
                    let id = v.targets[t];
                    let name = if t == 3 { NAMES[name].replace(".md", ".pdf") } else { NAMES[name].replace(".pdf", ".md").replace(".PDF", ".md") };
                    v.push(2, known, vec![OpBody::Meta(MetaOp::SetName { id, name })]);
                }
                Act::Move { t, f, .. } => {
                    let parent = f.map(|i| v.folders[i]);
                    v.push(3, known, vec![OpBody::Meta(MetaOp::SetParent { id: v.targets[t], parent })]);
                }
                Act::MoveFolder { f, into_root } => {
                    let parent = if into_root { None } else { Some(v.folders[(f + 1) % 3]) };
                    v.push(2, v.e.head, vec![OpBody::Meta(MetaOp::SetParent { id: v.folders[f], parent })]);
                }
                Act::AppendStaleLink { t_name } => {
                    // A replica that only knows the initial names appends a link to one of them.
                    v.sync_doc();
                    let old = ["Alpha", "Beta", "Gamma", "file.pdf"][t_name];
                    let len = ydoc::len16(&v.doc);
                    let u = ydoc::insert(&v.doc, len, &format!(" [[{old}]]"));
                    v.push(4, base_seq, vec![OpBody::Doc { entry: v.linker, slot: "body".into(), update: u }]);
                }
                Act::EditInsideLink { at, s } => {
                    // Concurrent edit authored against the *original* text (stale doc copy).
                    let stale = ydoc::new_doc(500 + v.op);
                    let init = v.e.doc_rows(v.linker, "body").unwrap();
                    ydoc::apply(&stale, &init[0]).unwrap();
                    let at = (at as u32).min(ydoc::len16(&stale));
                    let u = ydoc::insert(&stale, at, &s);
                    inserted.push_str(&s);
                    edited = true;
                    v.push(5, base_seq, vec![OpBody::Doc { entry: v.linker, slot: "body".into(), update: u }]);
                }
            }
            prop_assert!(v.e.state.check_invariants().is_empty());
        }
        let text = v.e.doc_text(v.linker, "body").unwrap();
        // Edits are never lost: every inserted character is still present.
        let mut hay: Vec<char> = text.chars().collect();
        for c in inserted.chars() {
            let pos = hay.iter().position(|x| *x == c);
            prop_assert!(pos.is_some(), "lost {:?} in {:?}", c, text);
            hay.remove(pos.unwrap());
        }
        // Without edits inside links, every original link still points at its original target.
        if !edited {
            let now = v.resolutions();
            for (i, o) in original.iter().enumerate() {
                prop_assert_eq!(now[i], *o, "link #{} in {:?}", i, text);
            }
        }
    }
}
