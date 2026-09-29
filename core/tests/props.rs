//! Property tests (DESIGN §17.2) for core semantics.

use jess_core::apply::{apply_meta, Ctx, Mode, Tx};
use jess_core::hlc::Hlc;
use jess_core::links::{extract, Link, Syntax};
use jess_core::model::{KIND_FOLDER, KIND_MARKDOWN, KIND_PDF};
use jess_core::ops::MetaOp;
use jess_core::resolve::{ResolveIndex, Via};
use jess_core::rewrite::format_target;
use jess_core::state::MetaState;
use jess_core::Id;
use proptest::prelude::*;

const SEGS: &[&str] = &["a", "A", "b", "sub dir", "Ünï", "x y"];
const FILES: &[&str] = &[
    "Note.md", "note.md", "x y.md", "img.PNG", "doc.pdf", "Ünï.md", "a.md",
];

fn path_strategy() -> impl Strategy<Value = String> {
    (
        prop::collection::vec(prop::sample::select(SEGS), 0..3),
        prop::sample::select(FILES),
    )
        .prop_map(|(dirs, f)| {
            let mut p = dirs.join("/");
            if !p.is_empty() {
                p.push('/');
            }
            p.push_str(f);
            p
        })
}

fn link_for(syntax: Syntax, text: &str, angle: bool) -> Link {
    Link {
        syntax,
        embed: false,
        target: text.into(),
        raw_target: text.into(),
        subpath: None,
        display: None,
        angle,
        range: (0, 0),
        target_range: (0, 0),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// Formatting a link for a target always resolves back to that target (§17.2).
    #[test]
    fn formatter_resolves_back(paths in prop::collection::btree_set(path_strategy(), 1..12), src_dir in prop::collection::vec(prop::sample::select(SEGS), 0..3), pick in any::<prop::sample::Index>(), style in 0..4u8) {
        let mut ix = ResolveIndex::new();
        let paths: Vec<String> = paths.into_iter().collect();
        for (i, p) in paths.iter().enumerate() {
            ix.insert(Id([i as u8 + 1; 16]), p);
        }
        let ti = pick.index(paths.len());
        let target = Id([ti as u8 + 1; 16]);
        let folder = src_dir.join("/");
        let (syntax, via, angle, text) = match style {
            0 => (Syntax::Wiki, Via::Basename, false, "x"),
            1 => (Syntax::Wiki, Via::Absolute, false, "q/x"),
            2 => (Syntax::Markdown, Via::Relative, false, "x.md"),
            _ => (Syntax::Markdown, Via::Relative, true, "x.md"),
        };
        let l = link_for(syntax, text, angle);
        let f = format_target(&ix, target, &l, via, &folder);
        let f = f.expect("a vault path always identifies an entry");
        let decoded = if syntax == Syntax::Markdown && !angle { percent_encoding::percent_decode_str(&f).decode_utf8().unwrap().to_string() } else { f.clone() };
        prop_assert_eq!(ix.resolve(&decoded, syntax, &folder).map(|r| r.id), Some(target), "formatted {:?} from {:?}", f, folder);
        // And the formatted text parses as a link with that target.
        let md = match (syntax, angle) {
            (Syntax::Wiki, _) => format!("[[{f}]]"),
            (Syntax::Markdown, true) => format!("[t](<{f}>)"),
            (Syntax::Markdown, false) => format!("[t]({f})"),
        };
        let ex = extract(&md);
        prop_assert_eq!(ex.links.len(), 1, "{}", md);
        prop_assert_eq!(&ex.links[0].target, &decoded);
    }

    /// Any sequence of meta ops keeps the tree valid (no cycles, unique live names, no live
    /// entry under a trashed parent), whatever order and clocks.
    #[test]
    fn apply_preserves_invariants(ops in prop::collection::vec((0..8u8, 0..6u8, 0..6u8, 0..7u8, 0..3u64, 0..1000u64), 1..60)) {
        let mut st = MetaState::new();
        let ids: Vec<Id> = (0..6).map(|i| Id([i + 1; 16])).collect();
        for (i, (kind, a, b, n, replica, wall)) in ops.into_iter().enumerate() {
            let id = ids[a as usize];
            let other = ids[b as usize];
            let op = match kind {
                0 => MetaOp::Create { id, kind: if a < 2 { KIND_FOLDER } else if a == 5 { KIND_PDF } else { KIND_MARKDOWN }.into(), parent: if b % 2 == 0 { None } else { Some(other) }, name: FILES[n as usize].into(), tree_visible: true, blob: None, blob_info: None, created_at: None, modified_at: None, props: vec![] },
                1 => MetaOp::SetName { id, name: FILES[n as usize].into() },
                2 => MetaOp::SetParent { id, parent: if b == 5 { None } else { Some(other) } },
                3 => MetaOp::Trash { id },
                4 => MetaOp::Restore { target: id },
                5 => MetaOp::Purge { id },
                6 => MetaOp::SetVisible { id, visible: n % 2 == 0 },
                _ => MetaOp::SetParent { id: other, parent: Some(id) },
            };
            let ctx = Ctx { hlc: Hlc::new(wall, 0, replica), known_seq: i as u64, replica, op_id: i as u64, mode: Mode::Server, now: wall };
            let mut tx = Tx::new();
            let _ = apply_meta(&mut st, &mut tx, &op, &ctx);
            let problems = st.check_invariants();
            prop_assert!(problems.is_empty(), "{:?} after {:?}", problems, op);
        }
    }

    /// LWW registers converge regardless of the order concurrent writes are applied in.
    #[test]
    fn lww_is_order_independent(writes in prop::collection::vec((0..3u64, 0..50u64, 0..7u8, any::<bool>()), 1..12)) {
        let id = Id([9; 16]);
        let create = MetaOp::Create { id, kind: KIND_PDF.into(), parent: None, name: "doc.pdf".into(), tree_visible: true, blob: None, blob_info: None, created_at: None, modified_at: None, props: vec![] };
        let run = |order: &[usize]| {
            let mut st = MetaState::new();
            let mut tx = Tx::new();
            apply_meta(&mut st, &mut tx, &create, &Ctx { hlc: Hlc::new(0, 0, 0), known_seq: 0, replica: 0, op_id: 0, mode: Mode::Server, now: 0 }).unwrap();
            for &k in order {
                let (replica, wall, n, vis) = writes[k];
                let ctx = Ctx { hlc: Hlc::new(wall + 1, k as u16, replica + 1), known_seq: 0, replica: replica + 1, op_id: k as u64, mode: Mode::Server, now: 0 };
                let _ = apply_meta(&mut st, &mut tx, &MetaOp::SetName { id, name: format!("n{n}.pdf") }, &ctx);
                let _ = apply_meta(&mut st, &mut tx, &MetaOp::SetVisible { id, visible: vis }, &ctx);
            }
            let e = st.get(&id).unwrap();
            (e.name.clone(), e.tree_visible)
        };
        let fwd: Vec<usize> = (0..writes.len()).collect();
        let rev: Vec<usize> = fwd.iter().rev().copied().collect();
        prop_assert_eq!(run(&fwd), run(&rev));
    }
}
