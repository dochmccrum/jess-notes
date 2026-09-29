//! `apply`: the single implementation of metadata semantics (DESIGN §6), run by the server
//! (canonical) and by clients (prediction for the optimistic view).

use crate::hlc::Hlc;
use crate::ids::Id;
use crate::model::{Clocks, Entry, Trashed, KIND_FOLDER, KIND_VAULT};
use crate::names::{first_free_suffix, is_storable_name, name_key, RECOVERED_PREFIX};
use crate::ops::{MetaOp, Reject};
use crate::state::MetaState;
use std::collections::{BTreeSet, HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Canonical application on the server.
    Server,
    /// Optimistic prediction on a client.
    Predict,
}

#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    pub hlc: Hlc,
    pub known_seq: u64,
    pub replica: u64,
    pub op_id: u64,
    pub mode: Mode,
    /// Wall time of the applier (server time on the server): used for `trashed.at`.
    pub now: u64,
}

/// A path change of one entry, recorded in `rename_history` (§6.6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameRec {
    pub entry: Id,
    pub old_path: String,
    pub new_path: String,
    pub is_folder: bool,
    /// The final name differs from what the op asked for (collision suffix, restore, create).
    pub induced: bool,
}

/// Undo log + effect collector for a batch of applies.
#[derive(Default, Debug)]
pub struct Tx {
    originals: HashMap<Id, Option<Entry>>,
    pub renames: Vec<RenameRec>,
    pub purged: Vec<Id>,
    /// Human-readable notes (e.g. "restored folder X because a note was created in it").
    pub notes: Vec<String>,
}

impl Tx {
    pub fn new() -> Tx {
        Tx::default()
    }
    fn touch(&mut self, st: &MetaState, id: Id) {
        self.originals
            .entry(id)
            .or_insert_with(|| st.get(&id).cloned());
    }
    /// Ids whose row changed (or was created) in this tx.
    pub fn changed(&self, st: &MetaState) -> Vec<Id> {
        let mut v: Vec<Id> = self
            .originals
            .iter()
            .filter(|(id, orig)| st.get(id) != orig.as_ref())
            .map(|(id, _)| *id)
            .collect();
        v.sort();
        v
    }
    pub fn original(&self, id: &Id) -> Option<&Option<Entry>> {
        self.originals.get(id)
    }
    pub fn touched(&self) -> impl Iterator<Item = &Id> {
        self.originals.keys()
    }
    /// Reverts every change since the tx (or the given savepoint) began.
    pub fn rollback(self, st: &mut MetaState) {
        for (id, orig) in self.originals {
            match orig {
                Some(e) => {
                    st.put(e);
                }
                None => {
                    st.remove(&id);
                }
            }
        }
    }
    /// Merge a nested tx (a successfully applied group) into this one.
    pub fn absorb(&mut self, inner: Tx) {
        for (id, orig) in inner.originals {
            self.originals.entry(id).or_insert(orig);
        }
        self.renames.extend(inner.renames);
        self.purged.extend(inner.purged);
        self.notes.extend(inner.notes);
    }
}

fn clocks_at(h: Hlc) -> Clocks {
    Clocks {
        parent: h,
        name: h,
        trashed: h,
        visible: h,
        blob: h,
        created: h,
        modified: h,
        props: Default::default(),
    }
}

fn trash_batch_id(ctx: &Ctx) -> Id {
    Id::derive(&[
        b"trash",
        &ctx.replica.to_le_bytes(),
        &ctx.op_id.to_le_bytes(),
    ])
}

struct Applier<'a> {
    st: &'a mut MetaState,
    tx: &'a mut Tx,
    ctx: &'a Ctx,
}

impl Applier<'_> {
    fn get(&self, id: Id) -> Result<&Entry, Reject> {
        self.st.get(&id).ok_or(Reject::UnknownEntry)
    }

    fn modify(&mut self, id: Id, f: impl FnOnce(&mut Entry)) {
        self.tx.touch(self.st, id);
        let mut e = self.st.get(&id).cloned().expect("modify existing");
        f(&mut e);
        self.st.put(e);
    }

    fn path(&self, id: Id) -> String {
        self.st.path_of(id).unwrap_or_default()
    }

    /// If the live entry `id` collides with another live entry, give it the smallest free suffix.
    /// Returns true if renamed.
    fn fix_collision(&mut self, id: Id) -> bool {
        let e = self.st.get(&id).expect("exists");
        if !e.is_live() || e.kind == KIND_VAULT {
            return false;
        }
        let key = name_key(&e.name);
        if !self.st.name_taken(e.parent, &key, id) {
            return false;
        }
        let (parent, name, folder) = (e.parent, e.name.clone(), e.is_folder());
        let st = &*self.st;
        let new_name = first_free_suffix(&name, folder, |k| st.name_taken(parent, k, id));
        let h = self.ctx.hlc;
        self.modify(id, |e| {
            e.name = new_name;
            if h > e.clock.name {
                e.clock.name = h;
            }
        });
        true
    }

    /// Restores `start` and its trashed ancestors (only that chain, §6.4), top-down.
    fn restore_chain(&mut self, start: Id, why: &str) {
        let mut chain = Vec::new();
        let mut cur = Some(start);
        while let Some(c) = cur {
            match self.st.get(&c) {
                Some(e) if e.trashed.is_some() && !e.purged => {
                    chain.push(c);
                    cur = e.parent;
                }
                _ => break,
            }
        }
        for id in chain.into_iter().rev() {
            self.restore_one(id, why);
        }
    }

    fn restore_one(&mut self, id: Id, why: &str) {
        let before = self.path(id);
        let h = self.ctx.hlc;
        // A restored entry whose parent vanished goes to the root.
        let parent_ok = match self.st.get(&id).and_then(|e| e.parent) {
            None => true,
            Some(p) => self
                .st
                .get(&p)
                .map(|pe| !pe.purged && pe.is_folder())
                .unwrap_or(false),
        };
        self.modify(id, |e| {
            e.trashed = None;
            if h > e.clock.trashed {
                e.clock.trashed = h;
            }
            if !parent_ok {
                e.parent = None;
                if h > e.clock.parent {
                    e.clock.parent = h;
                }
            }
        });
        if let Some(p) = self.st.get(&id).and_then(|e| e.parent) {
            if self
                .st
                .get(&p)
                .map(|pe| pe.trashed.is_some())
                .unwrap_or(false)
            {
                self.restore_chain(p, why);
            }
        }
        let renamed = self.fix_collision(id);
        let after = self.path(id);
        if renamed || !parent_ok {
            self.record_rename(id, before, after, true);
        }
        if !why.is_empty() {
            let msg = format!("restored {} ({why})", self.path(id));
            self.tx.notes.push(msg);
        }
    }

    fn record_rename(&mut self, id: Id, old_path: String, new_path: String, induced: bool) {
        if old_path == new_path {
            return;
        }
        let is_folder = self.st.get(&id).map(|e| e.is_folder()).unwrap_or(false);
        self.tx.renames.push(RenameRec {
            entry: id,
            old_path,
            new_path,
            is_folder,
            induced,
        });
    }

    /// Validates `parent` as a destination for `id` (None = root). Returns the effective parent.
    fn check_parent(
        &mut self,
        id: Id,
        parent: Option<Id>,
        for_create: bool,
    ) -> Result<Option<Id>, Reject> {
        let Some(p) = parent else { return Ok(None) };
        if p == id {
            return Err(Reject::Cycle);
        }
        let pe = match self.st.get(&p) {
            Some(pe) if !pe.purged => pe,
            _ => {
                // Keep data: a create whose parent is gone lands at the root.
                return if for_create {
                    Ok(None)
                } else {
                    Err(Reject::ParentMissing)
                };
            }
        };
        if !pe.is_folder() {
            return if for_create {
                Ok(None)
            } else {
                Err(Reject::NotAFolder)
            };
        }
        if !for_create && self.st.is_ancestor_or_self(id, p) {
            return Err(Reject::Cycle);
        }
        if pe.trashed.is_some() {
            let moving_live = self.st.get(&id).map(|e| e.is_live()).unwrap_or(true);
            if moving_live {
                // Concurrent trash (the op didn't know the parent's latest state): restore the chain.
                let concurrent = pe.seq > self.ctx.known_seq || for_create;
                if concurrent && self.ctx.mode == Mode::Server || for_create {
                    self.restore_chain(p, "an entry was created or moved into it concurrently");
                } else {
                    return Err(Reject::ParentTrashed);
                }
            }
        }
        Ok(Some(p))
    }

    fn apply(&mut self, op: &MetaOp) -> Result<(), Reject> {
        let h = self.ctx.hlc;
        match op {
            MetaOp::Create {
                id,
                kind,
                parent,
                name,
                tree_visible,
                blob,
                blob_info: _,
                created_at,
                modified_at,
                props,
            } => {
                if !is_storable_name(name) && kind != KIND_VAULT {
                    return Err(Reject::BadName);
                }
                if let Some(e) = self.st.get(id) {
                    if e.purged {
                        return Err(Reject::Purged);
                    }
                    // Replayed/duplicate create of an existing id: field-wise LWW.
                    if kind == KIND_VAULT {
                        return Ok(());
                    }
                    self.apply(&MetaOp::SetParent {
                        id: *id,
                        parent: *parent,
                    })?;
                    self.apply(&MetaOp::SetName {
                        id: *id,
                        name: name.clone(),
                    })?;
                    return Ok(());
                }
                if kind == KIND_VAULT && (*id != crate::ids::VAULT_SETTINGS_ID || parent.is_some())
                {
                    return Err(Reject::Forbidden);
                }
                if *id == crate::ids::VAULT_SETTINGS_ID && kind != KIND_VAULT {
                    return Err(Reject::Forbidden);
                }
                let parent = self.check_parent(*id, *parent, true)?;
                let mut clock = clocks_at(h);
                let mut pm = std::collections::BTreeMap::new();
                for (k, v) in props {
                    clock.props.insert(k.clone(), h);
                    pm.insert(k.clone(), v.clone());
                }
                let e = Entry {
                    id: *id,
                    kind: kind.clone(),
                    parent,
                    name: name.clone(),
                    trashed: None,
                    tree_visible: *tree_visible,
                    blob: if kind == KIND_FOLDER { None } else { *blob },
                    created_at: *created_at,
                    modified_at: *modified_at,
                    props: pm,
                    purged: false,
                    clock,
                    seq: 0,
                };
                self.tx.touch(self.st, *id);
                self.st.put(e);
                let requested = match parent {
                    Some(p) => format!("{}/{}", self.path(p), name),
                    None => name.clone(),
                };
                if self.fix_collision(*id) {
                    let after = self.path(*id);
                    self.record_rename(*id, requested, after, true);
                }
                Ok(())
            }
            MetaOp::SetParent { id, parent } => {
                let e = self.get(*id)?;
                if e.purged {
                    return Err(Reject::Purged);
                }
                if e.kind == KIND_VAULT {
                    return Err(Reject::Forbidden);
                }
                if h <= e.clock.parent || e.parent == *parent {
                    if h > e.clock.parent {
                        self.modify(*id, |e| e.clock.parent = h);
                    }
                    return Ok(());
                }
                let before = self.path(*id);
                let parent = self.check_parent(*id, *parent, false)?;
                self.modify(*id, |e| {
                    e.parent = parent;
                    e.clock.parent = h;
                });
                let induced = self.fix_collision(*id);
                let after = self.path(*id);
                // Trashed entries too: links inside trashed notes keep their meaning (Invariant R).
                self.record_rename(*id, before, after, induced);
                Ok(())
            }
            MetaOp::SetName { id, name } => {
                let e = self.get(*id)?;
                if e.purged {
                    return Err(Reject::Purged);
                }
                if e.kind == KIND_VAULT {
                    return Err(Reject::Forbidden);
                }
                if !is_storable_name(name) {
                    return Err(Reject::BadName);
                }
                if h <= e.clock.name || e.name == *name {
                    if h > e.clock.name {
                        self.modify(*id, |e| e.clock.name = h);
                    }
                    return Ok(());
                }
                let before = self.path(*id);
                self.modify(*id, |e| {
                    e.name = name.clone();
                    e.clock.name = h;
                });
                let induced = self.fix_collision(*id);
                let after = self.path(*id);
                // Trashed entries too: links inside trashed notes keep their meaning (Invariant R).
                self.record_rename(*id, before, after, induced);
                Ok(())
            }
            MetaOp::SetVisible { id, visible } => {
                let e = self.get(*id)?;
                if e.purged {
                    return Err(Reject::Purged);
                }
                if h > e.clock.visible {
                    self.modify(*id, |e| {
                        e.tree_visible = *visible;
                        e.clock.visible = h;
                    });
                }
                Ok(())
            }
            MetaOp::SetBlob { id, blob, .. } => {
                let e = self.get(*id)?;
                if e.purged {
                    return Err(Reject::Purged);
                }
                if e.is_folder() || e.kind == KIND_VAULT {
                    return Err(Reject::Forbidden);
                }
                if h > e.clock.blob {
                    self.modify(*id, |e| {
                        e.blob = Some(*blob);
                        e.clock.blob = h;
                    });
                }
                Ok(())
            }
            MetaOp::SetProp { id, key, value } => {
                let e = self.get(*id)?;
                if e.purged {
                    return Err(Reject::Purged);
                }
                if e.clock.props.get(key).map(|c| h > *c).unwrap_or(true) {
                    self.modify(*id, |e| {
                        match value {
                            Some(v) => {
                                e.props.insert(key.clone(), v.clone());
                            }
                            None => {
                                e.props.remove(key);
                            }
                        }
                        e.clock.props.insert(key.clone(), h);
                    });
                }
                Ok(())
            }
            MetaOp::SetTimes {
                id,
                created,
                modified,
            } => {
                let e = self.get(*id)?;
                if e.purged {
                    return Err(Reject::Purged);
                }
                let (cc, mc) = (e.clock.created, e.clock.modified);
                self.modify(*id, |e| {
                    if created.is_some() && h > cc {
                        e.created_at = *created;
                        e.clock.created = h;
                    }
                    if modified.is_some() && h > mc {
                        e.modified_at = *modified;
                        e.clock.modified = h;
                    }
                });
                Ok(())
            }
            MetaOp::Trash { id } => {
                let e = self.get(*id)?;
                if e.purged {
                    return Err(Reject::Purged);
                }
                if e.kind == KIND_VAULT {
                    return Err(Reject::Forbidden);
                }
                if e.trashed.is_some() || h <= e.clock.trashed {
                    return Ok(());
                }
                let t = Trashed {
                    batch: trash_batch_id(self.ctx),
                    at: self.ctx.now,
                };
                let mut ids = vec![*id];
                ids.extend(
                    self.st
                        .descendants(*id)
                        .into_iter()
                        .filter(|d| self.st.get(d).map(|x| x.trashed.is_none()).unwrap_or(false)),
                );
                for d in ids {
                    // The cascade is structural: descendants follow their folder regardless of clocks.
                    self.modify(d, |e| {
                        e.trashed = Some(t);
                        if h > e.clock.trashed {
                            e.clock.trashed = h;
                        }
                    });
                }
                Ok(())
            }
            MetaOp::Restore { target } => {
                let members: Vec<Id> = if let Some(e) = self.st.get(target) {
                    if e.purged {
                        return Err(Reject::Purged);
                    }
                    let Some(t) = e.trashed else { return Ok(()) };
                    if h <= e.clock.trashed {
                        return Ok(());
                    }
                    let mut v = vec![*target];
                    v.extend(self.st.descendants(*target).into_iter().filter(|d| {
                        self.st.get(d).and_then(|x| x.trashed).map(|x| x.batch) == Some(t.batch)
                    }));
                    v
                } else {
                    let v = self.st.batch_members(*target);
                    if v.is_empty() {
                        return Err(Reject::UnknownEntry);
                    }
                    v
                };
                // Parents before children so collisions are checked against the final tree.
                let mut members: Vec<(usize, Id)> = members
                    .into_iter()
                    .map(|id| (self.path(id).matches('/').count(), id))
                    .collect();
                members.sort();
                for (_, id) in members {
                    if self
                        .st
                        .get(&id)
                        .map(|e| e.trashed.is_some())
                        .unwrap_or(false)
                    {
                        self.restore_one(id, "");
                    }
                }
                Ok(())
            }
            MetaOp::Purge { id } => {
                let e = self.get(*id)?;
                if e.purged {
                    return Ok(());
                }
                if e.trashed.is_none() {
                    return Err(Reject::NotTrashed);
                }
                let mut ids = vec![*id];
                ids.extend(self.st.descendants(*id));
                for d in ids.into_iter().rev() {
                    let live = self.st.get(&d).map(|x| x.is_live()).unwrap_or(false);
                    if live {
                        // Cannot happen with a consistent tree; keep data by moving it to the root.
                        self.modify(d, |e| e.parent = None);
                        self.fix_collision(d);
                        continue;
                    }
                    self.modify(d, |e| {
                        e.purged = true;
                    });
                    self.tx.purged.push(d);
                }
                Ok(())
            }
        }
    }
}

/// Applies one meta op. On `Err` the state may be partially modified; callers roll back via `Tx`.
pub fn apply_meta(st: &mut MetaState, tx: &mut Tx, op: &MetaOp, ctx: &Ctx) -> Result<(), Reject> {
    let mut inner = Tx::new();
    let r = Applier {
        st,
        tx: &mut inner,
        ctx,
    }
    .apply(op);
    match r {
        Ok(()) => {
            tx.absorb(inner);
            Ok(())
        }
        Err(e) => {
            inner.rollback(st);
            Err(e)
        }
    }
}

/// Recovered-after-purge (§6.4): brings a purged tombstone back into trash as "Recovered — Name".
pub fn recover_purged(st: &mut MetaState, tx: &mut Tx, id: Id, ctx: &Ctx) -> bool {
    let Some(e) = st.get(&id) else { return false };
    if !e.purged {
        return false;
    }
    let parent_ok = e
        .parent
        .and_then(|p| st.get(&p))
        .map(|p| !p.purged && p.is_folder())
        .unwrap_or(true);
    let h = ctx.hlc;
    tx.touch(st, id);
    let mut e = e.clone();
    e.purged = false;
    if !e.name.starts_with(RECOVERED_PREFIX) {
        e.name = format!("{RECOVERED_PREFIX}{}", e.name);
    }
    if !parent_ok {
        e.parent = None;
    }
    e.trashed = Some(Trashed {
        batch: Id::derive(&[b"recover", id.as_bytes(), &h.wall.to_le_bytes()]),
        at: ctx.now,
    });
    e.clock = clocks_at(h.max(e.clock.name));
    st.put(e);
    tx.notes
        .push(format!("recovered purged entry {id} into trash"));
    true
}

/// All live, non-purged ids in a subtree (including root).
pub fn subtree(st: &MetaState, id: Id) -> BTreeSet<Id> {
    let mut s: BTreeSet<Id> = st.descendants(id).into_iter().collect();
    s.insert(id);
    s
}

#[cfg(test)]
mod tests;
