//! In-memory metadata state with the indexes `apply` and the resolver need.

use crate::ids::Id;
use crate::model::{Entry, KIND_VAULT};
use crate::names::name_key;
use std::collections::{BTreeSet, HashMap};

#[derive(Clone, Debug, Default)]
pub struct MetaState {
    entries: HashMap<Id, Entry>,
    /// (parent, NFC name) → live entries with that key (normally one).
    live_names: HashMap<(Option<Id>, String), Vec<Id>>,
    /// parent → non-purged children (live or trashed).
    children: HashMap<Option<Id>, BTreeSet<Id>>,
}

impl MetaState {
    pub fn new() -> MetaState {
        MetaState::default()
    }

    pub fn from_entries(entries: impl IntoIterator<Item = Entry>) -> MetaState {
        let mut s = MetaState::new();
        for e in entries {
            s.put(e);
        }
        s
    }

    pub fn get(&self, id: &Id) -> Option<&Entry> {
        self.entries.get(id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    fn unindex(&mut self, e: &Entry) {
        if e.purged {
            return;
        }
        if let Some(set) = self.children.get_mut(&e.parent) {
            set.remove(&e.id);
            if set.is_empty() {
                self.children.remove(&e.parent);
            }
        }
        if e.is_live() && e.kind != KIND_VAULT {
            let k = (e.parent, name_key(&e.name));
            if let Some(v) = self.live_names.get_mut(&k) {
                v.retain(|x| *x != e.id);
                if v.is_empty() {
                    self.live_names.remove(&k);
                }
            }
        }
    }

    fn index(&mut self, e: &Entry) {
        if e.purged {
            return;
        }
        self.children.entry(e.parent).or_default().insert(e.id);
        if e.is_live() && e.kind != KIND_VAULT {
            let v = self
                .live_names
                .entry((e.parent, name_key(&e.name)))
                .or_default();
            if !v.contains(&e.id) {
                v.push(e.id);
                v.sort();
            }
        }
    }

    /// Inserts or replaces an entry, maintaining indexes.
    pub fn put(&mut self, e: Entry) -> Option<Entry> {
        let old = self.entries.remove(&e.id);
        if let Some(o) = &old {
            self.unindex(o);
        }
        self.index(&e);
        self.entries.insert(e.id, e);
        old
    }

    pub fn remove(&mut self, id: &Id) -> Option<Entry> {
        let old = self.entries.remove(id);
        if let Some(o) = &old {
            self.unindex(o);
        }
        old
    }

    /// The live entry occupying `(parent, name_key)`, if any, other than `except`.
    pub fn live_by_name(&self, parent: Option<Id>, key: &str, except: Option<Id>) -> Option<Id> {
        self.live_names
            .get(&(parent, key.to_string()))
            .and_then(|v| v.iter().copied().find(|x| Some(*x) != except))
    }

    pub fn name_taken(&self, parent: Option<Id>, key: &str, except: Id) -> bool {
        self.live_by_name(parent, key, Some(except)).is_some()
    }

    /// Non-purged children (live and trashed).
    pub fn children(&self, parent: Option<Id>) -> impl Iterator<Item = Id> + '_ {
        self.children
            .get(&parent)
            .into_iter()
            .flat_map(|s| s.iter().copied())
    }

    /// All non-purged descendants of `id` (excluding `id`), parents before children.
    pub fn descendants(&self, id: Id) -> Vec<Id> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(x) = stack.pop() {
            for c in self.children(Some(x)) {
                out.push(c);
                stack.push(c);
            }
        }
        out
    }

    /// True if `anc` is `id` or one of its ancestors.
    pub fn is_ancestor_or_self(&self, anc: Id, id: Id) -> bool {
        let mut cur = Some(id);
        let mut guard = 0;
        while let Some(c) = cur {
            if c == anc {
                return true;
            }
            cur = self.entries.get(&c).and_then(|e| e.parent);
            guard += 1;
            if guard > 100_000 {
                return true; // corrupt (cyclic) state: treat as cycle
            }
        }
        false
    }

    /// Vault-relative path (`a/b/Note.md`) following parent pointers.
    pub fn path_of(&self, id: Id) -> Option<String> {
        let mut parts = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            let e = self.entries.get(&c)?;
            parts.push(e.name.as_str());
            cur = e.parent;
            if parts.len() > 10_000 {
                return None;
            }
        }
        parts.reverse();
        Some(parts.join("/"))
    }

    /// Folder path of an entry's parent ("" for root).
    pub fn folder_of(&self, id: Id) -> String {
        match self.entries.get(&id).and_then(|e| e.parent) {
            Some(p) => self.path_of(p).unwrap_or_default(),
            None => String::new(),
        }
    }

    /// Looks up a live entry by exact (case-sensitive, NFC) vault path.
    pub fn by_path(&self, path: &str) -> Option<Id> {
        let mut parent = None;
        let mut found = None;
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            let id = self.live_by_name(parent, &name_key(seg), None)?;
            parent = Some(id);
            found = Some(id);
        }
        found
    }

    /// Entries whose `trashed.batch == batch`.
    pub fn batch_members(&self, batch: Id) -> Vec<Id> {
        let mut v: Vec<Id> = self
            .entries
            .values()
            .filter(|e| e.trashed.map(|t| t.batch) == Some(batch))
            .map(|e| e.id)
            .collect();
        v.sort();
        v
    }

    pub fn max_seq(&self) -> u64 {
        self.entries.values().map(|e| e.seq).max().unwrap_or(0)
    }

    /// Structural invariants; returns human-readable problems (used by tests and integrity-check).
    pub fn check_invariants(&self) -> Vec<String> {
        let mut out = Vec::new();
        for ((p, k), v) in &self.live_names {
            if v.len() > 1 {
                out.push(format!("duplicate live name {k:?} under {p:?}: {v:?}"));
            }
        }
        for e in self.entries.values() {
            if e.purged {
                continue;
            }
            if let Some(p) = e.parent {
                match self.entries.get(&p) {
                    None => out.push(format!("{:?} has missing parent {:?}", e.id, p)),
                    Some(pe) => {
                        if pe.purged {
                            out.push(format!("{:?} has purged parent", e.id));
                        }
                        if !pe.is_folder() {
                            out.push(format!("{:?} parent is not a folder", e.id));
                        }
                        if e.is_live() && !pe.is_live() {
                            out.push(format!("live {:?} ({}) under trashed parent", e.id, e.name));
                        }
                    }
                }
                if self.is_ancestor_or_self(e.id, p) {
                    out.push(format!("cycle at {:?}", e.id));
                }
            }
        }
        out
    }
}
