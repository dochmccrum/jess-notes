use super::*;
use crate::model::KIND_MARKDOWN;

fn id(n: u8) -> Id {
    Id([n; 16])
}

struct T {
    st: MetaState,
    t: u64,
}

impl T {
    fn new() -> T {
        T {
            st: MetaState::new(),
            t: 100,
        }
    }
    fn ctx(&mut self, replica: u64, known_seq: u64) -> Ctx {
        self.t += 1;
        Ctx {
            hlc: Hlc::new(self.t, 0, replica),
            known_seq,
            replica,
            op_id: self.t,
            mode: Mode::Server,
            now: 0,
        }
    }
    fn run(&mut self, op: MetaOp) -> Result<Tx, Reject> {
        let c = self.ctx(1, u64::MAX);
        self.run_ctx(op, c)
    }
    fn run_ctx(&mut self, op: MetaOp, c: Ctx) -> Result<Tx, Reject> {
        let mut tx = Tx::new();
        apply_meta(&mut self.st, &mut tx, &op, &c)?;
        assert!(
            self.st.check_invariants().is_empty(),
            "{:?}",
            self.st.check_invariants()
        );
        Ok(tx)
    }
    fn create(&mut self, i: u8, parent: Option<u8>, name: &str, folder: bool) -> Tx {
        self.run(MetaOp::Create {
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
        })
        .unwrap()
    }
    fn name(&self, i: u8) -> String {
        self.st.get(&id(i)).unwrap().name.clone()
    }
}

#[test]
fn collision_suffix_on_create_and_rename() {
    let mut t = T::new();
    t.create(1, None, "Untitled.md", false);
    let tx = t.create(2, None, "Untitled.md", false);
    assert_eq!(t.name(2), "Untitled 1.md");
    assert_eq!(tx.renames.len(), 1);
    assert!(tx.renames[0].induced);
    t.create(3, None, "x.md", false);
    t.run(MetaOp::SetName {
        id: id(3),
        name: "Untitled.md".into(),
    })
    .unwrap();
    assert_eq!(t.name(3), "Untitled 2.md");
    // case-sensitive uniqueness (D8)
    t.create(4, None, "untitled.md", false);
    assert_eq!(t.name(4), "untitled.md");
}

#[test]
fn cycle_rejected() {
    let mut t = T::new();
    t.create(1, None, "A", true);
    t.create(2, Some(1), "B", true);
    assert_eq!(
        t.run(MetaOp::SetParent {
            id: id(1),
            parent: Some(id(2))
        })
        .err(),
        Some(Reject::Cycle)
    );
    assert_eq!(
        t.run(MetaOp::SetParent {
            id: id(1),
            parent: Some(id(1))
        })
        .err(),
        Some(Reject::Cycle)
    );
}

#[test]
fn lww_older_write_loses() {
    let mut t = T::new();
    t.create(1, None, "a.md", false);
    let newer = Ctx {
        hlc: Hlc::new(1000, 0, 2),
        known_seq: 0,
        replica: 2,
        op_id: 1,
        mode: Mode::Server,
        now: 0,
    };
    let older = Ctx {
        hlc: Hlc::new(500, 0, 3),
        known_seq: 0,
        replica: 3,
        op_id: 1,
        mode: Mode::Server,
        now: 0,
    };
    t.run_ctx(
        MetaOp::SetName {
            id: id(1),
            name: "new.md".into(),
        },
        newer,
    )
    .unwrap();
    t.run_ctx(
        MetaOp::SetName {
            id: id(1),
            name: "old.md".into(),
        },
        older,
    )
    .unwrap();
    assert_eq!(t.name(1), "new.md");
}

#[test]
fn trash_cascade_restore_batch() {
    let mut t = T::new();
    t.create(1, None, "F", true);
    t.create(2, Some(1), "n.md", false);
    t.create(3, Some(1), "m.md", false);
    t.run(MetaOp::Trash { id: id(3) }).unwrap();
    t.run(MetaOp::Trash { id: id(1) }).unwrap();
    let b1 = t.st.get(&id(1)).unwrap().trashed.unwrap().batch;
    assert_eq!(t.st.get(&id(2)).unwrap().trashed.unwrap().batch, b1);
    assert_ne!(t.st.get(&id(3)).unwrap().trashed.unwrap().batch, b1);
    t.run(MetaOp::Restore { target: b1 }).unwrap();
    assert!(t.st.get(&id(1)).unwrap().is_live());
    assert!(t.st.get(&id(2)).unwrap().is_live());
    assert!(
        !t.st.get(&id(3)).unwrap().is_live(),
        "restore restores exactly the batch"
    );
}

#[test]
fn create_in_trashed_folder_restores_only_chain() {
    let mut t = T::new();
    t.create(1, None, "F", true);
    t.create(2, Some(1), "G", true);
    t.create(3, Some(2), "old.md", false);
    t.create(4, Some(1), "sib.md", false);
    t.run(MetaOp::Trash { id: id(1) }).unwrap();
    t.create(5, Some(2), "new.md", false);
    assert!(t.st.get(&id(1)).unwrap().is_live());
    assert!(t.st.get(&id(2)).unwrap().is_live());
    assert!(t.st.get(&id(5)).unwrap().is_live());
    assert!(!t.st.get(&id(3)).unwrap().is_live());
    assert!(!t.st.get(&id(4)).unwrap().is_live());
}

#[test]
fn move_into_trashed_known_vs_concurrent() {
    let mut t = T::new();
    t.create(1, None, "F", true);
    t.create(2, None, "n.md", false);
    t.run(MetaOp::Trash { id: id(1) }).unwrap();
    t.st.put({
        let mut e = t.st.get(&id(1)).unwrap().clone();
        e.seq = 10;
        e
    });
    let known = t.ctx(1, 10);
    assert_eq!(
        t.run_ctx(
            MetaOp::SetParent {
                id: id(2),
                parent: Some(id(1))
            },
            known
        )
        .err(),
        Some(Reject::ParentTrashed)
    );
    let concurrent = t.ctx(1, 5);
    t.run_ctx(
        MetaOp::SetParent {
            id: id(2),
            parent: Some(id(1)),
        },
        concurrent,
    )
    .unwrap();
    assert!(t.st.get(&id(1)).unwrap().is_live());
}

#[test]
fn purge_and_recover() {
    let mut t = T::new();
    t.create(1, None, "n.md", false);
    assert_eq!(
        t.run(MetaOp::Purge { id: id(1) }).err(),
        Some(Reject::NotTrashed)
    );
    t.run(MetaOp::Trash { id: id(1) }).unwrap();
    let tx = t.run(MetaOp::Purge { id: id(1) }).unwrap();
    assert_eq!(tx.purged, vec![id(1)]);
    assert_eq!(
        t.run(MetaOp::SetName {
            id: id(1),
            name: "x.md".into()
        })
        .err(),
        Some(Reject::Purged)
    );
    let c = t.ctx(1, 0);
    let mut tx = Tx::new();
    assert!(recover_purged(&mut t.st, &mut tx, id(1), &c));
    let e = t.st.get(&id(1)).unwrap();
    assert!(e.trashed.is_some() && !e.purged);
    assert_eq!(e.name, "Recovered — n.md");
}

#[test]
fn restore_collision_gets_suffix() {
    let mut t = T::new();
    t.create(1, None, "a.md", false);
    t.run(MetaOp::Trash { id: id(1) }).unwrap();
    t.create(2, None, "a.md", false);
    t.run(MetaOp::Restore { target: id(1) }).unwrap();
    assert_eq!(t.name(1), "a 1.md");
    assert_eq!(t.name(2), "a.md");
}

#[test]
fn rollback_restores_state() {
    let mut t = T::new();
    t.create(1, None, "a.md", false);
    let before = t.st.get(&id(1)).cloned();
    let mut tx = Tx::new();
    let c = t.ctx(1, 0);
    apply_meta(
        &mut t.st,
        &mut tx,
        &MetaOp::SetName {
            id: id(1),
            name: "b.md".into(),
        },
        &c,
    )
    .unwrap();
    tx.rollback(&mut t.st);
    assert_eq!(t.st.get(&id(1)).cloned(), before);
}
