//! Link resolution (DESIGN §9.2). Case-insensitive, NFC; only live non-folder entries count.

use crate::ids::Id;
use crate::links::{is_external, Syntax};
use crate::names::lookup_key;
use crate::state::MetaState;
use std::collections::HashMap;

/// Which rule matched; the rewrite formatter reproduces the same style (§6.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    Relative,
    Absolute,
    Suffix,
    Basename,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub id: Id,
    pub via: Via,
}

/// Incrementally maintained lookup tables over linkable entries.
#[derive(Clone, Debug, Default)]
pub struct ResolveIndex {
    by_path: HashMap<String, Vec<Id>>,
    by_name: HashMap<String, Vec<Id>>,
    paths: HashMap<Id, String>,
}

fn name_keys(path_lower: &str) -> Vec<String> {
    let base = path_lower.rsplit('/').next().unwrap_or(path_lower);
    let mut v = vec![base.to_string()];
    if let Some(s) = base.strip_suffix(".md") {
        v.push(s.to_string());
    }
    v
}

fn push(map: &mut HashMap<String, Vec<Id>>, k: String, id: Id) {
    let v = map.entry(k).or_default();
    if !v.contains(&id) {
        v.push(id);
    }
}

fn pull(map: &mut HashMap<String, Vec<Id>>, k: &str, id: Id) {
    if let Some(v) = map.get_mut(k) {
        v.retain(|x| *x != id);
        if v.is_empty() {
            map.remove(k);
        }
    }
}

impl ResolveIndex {
    pub fn new() -> ResolveIndex {
        ResolveIndex::default()
    }

    pub fn build(st: &MetaState) -> ResolveIndex {
        let mut ix = ResolveIndex::new();
        for e in st.iter() {
            if e.is_linkable() {
                if let Some(p) = st.path_of(e.id) {
                    ix.insert(e.id, &p);
                }
            }
        }
        ix
    }

    pub fn insert(&mut self, id: Id, path: &str) {
        self.remove(id);
        let pl = lookup_key(path);
        for k in name_keys(&pl) {
            push(&mut self.by_name, k, id);
        }
        push(&mut self.by_path, pl, id);
        self.paths.insert(id, path.to_string());
    }

    pub fn remove(&mut self, id: Id) {
        if let Some(p) = self.paths.remove(&id) {
            let pl = lookup_key(&p);
            for k in name_keys(&pl) {
                pull(&mut self.by_name, &k, id);
            }
            pull(&mut self.by_path, &pl, id);
        }
    }

    /// Re-indexes `ids` (and descendants of folders among them) from `st`.
    pub fn sync(&mut self, st: &MetaState, ids: &[Id]) {
        let mut stack: Vec<Id> = ids.to_vec();
        let mut seen = std::collections::HashSet::new();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            match st.get(&id) {
                Some(e) if e.is_linkable() => match st.path_of(id) {
                    Some(p) => self.insert(id, &p),
                    None => self.remove(id),
                },
                Some(e) if e.is_folder() => {
                    self.remove(id);
                    stack.extend(st.children(Some(id)));
                }
                _ => self.remove(id),
            }
        }
    }

    pub fn path(&self, id: &Id) -> Option<&str> {
        self.paths.get(id).map(|s| s.as_str())
    }

    pub fn contains(&self, id: &Id) -> bool {
        self.paths.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    fn exact(&self, key: &str) -> Vec<Id> {
        let mut v: Vec<Id> = self.by_path.get(key).cloned().unwrap_or_default();
        if let Some(md) = self.by_path.get(&format!("{key}.md")) {
            v.extend(md.iter().copied());
        }
        v
    }

    fn pick(&self, mut cands: Vec<Id>, source_folder_lower: &str, exact: &str) -> Option<Id> {
        cands.sort();
        cands.dedup();
        cands.into_iter().min_by(|a, b| {
            self.rank(a, source_folder_lower, exact)
                .cmp(&self.rank(b, source_folder_lower, exact))
        })
    }

    /// Tie-break (§9.2 rule 7): own folder, then an exact-case match (D8 allows `a.md` and
    /// `A.md` side by side), then fewest segments, shortest path, lexicographic path.
    fn rank(
        &self,
        id: &Id,
        source_folder_lower: &str,
        exact: &str,
    ) -> (bool, bool, usize, usize, String, Id) {
        let p = self.paths.get(id).map(|s| s.as_str()).unwrap_or("");
        let pl = lookup_key(p);
        let folder = pl.rsplit_once('/').map(|(f, _)| f).unwrap_or("");
        let pn = crate::names::name_key(p);
        let ends = |suffix: &str| pn == suffix || pn.ends_with(&format!("/{suffix}"));
        let exact_case = !exact.is_empty() && (ends(exact) || ends(&format!("{exact}.md")));
        (
            folder != source_folder_lower,
            !exact_case,
            pl.matches('/').count(),
            p.len(),
            p.to_string(),
            *id,
        )
    }

    /// Resolves `target` (as written) from a note in `source_folder` (vault path, "" = root).
    pub fn resolve(&self, target: &str, syntax: Syntax, source_folder: &str) -> Option<Resolved> {
        let t = target.trim();
        if t.is_empty() || is_external(t) {
            return None;
        }
        let key = lookup_key(t);
        let folder = lookup_key(source_folder);
        let tn = crate::names::name_key(t);
        let exact_abs = tn.trim_start_matches('/').to_string();
        let exact_rel = join(&crate::names::name_key(source_folder), &tn).unwrap_or_default();
        let pick = |cands: Vec<Id>, via: Via, exact: &str| {
            self.pick(cands, &folder, exact)
                .map(|id| Resolved { id, via })
        };
        let one = |cands: Vec<Id>, via: Via| match via {
            Via::Relative => pick(cands, via, &exact_rel),
            _ => pick(cands, via, &exact_abs),
        };
        if syntax == Syntax::Markdown {
            if let Some(rel) = join(&folder, &key) {
                if let Some(r) = one(self.exact(&rel), Via::Relative) {
                    return Some(r);
                }
            }
            if let Some(r) = one(self.exact(key.trim_start_matches('/')), Via::Absolute) {
                return Some(r);
            }
        }
        if key.starts_with("./") || key.starts_with("../") {
            return join(&folder, &key).and_then(|rel| one(self.exact(&rel), Via::Relative));
        }
        if let Some(abs) = key.strip_prefix('/') {
            return one(self.exact(abs), Via::Absolute);
        }
        if key.contains('/') {
            if let Some(r) = one(self.exact(&key), Via::Absolute) {
                return Some(r);
            }
            if let Some(rel) = join(&folder, &key) {
                if let Some(r) = one(self.exact(&rel), Via::Relative) {
                    return Some(r);
                }
            }
            let base = key.rsplit('/').next().unwrap_or(&key);
            let cands: Vec<Id> = self
                .by_name
                .get(base)
                .into_iter()
                .flatten()
                .copied()
                .filter(|id| {
                    let pl = lookup_key(self.paths.get(id).map(|s| s.as_str()).unwrap_or(""));
                    pl.ends_with(&format!("/{key}")) || pl.ends_with(&format!("/{key}.md"))
                })
                .collect();
            return one(cands, Via::Suffix);
        }
        one(
            self.by_name.get(&key).cloned().unwrap_or_default(),
            Via::Basename,
        )
    }
}

/// Joins a folder and a relative path, normalising `.` and `..`. `None` if `..` escapes the vault.
pub fn join(folder: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = folder.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// Relative path from `from_folder` to `to_path` (both vault paths).
pub fn relative(from_folder: &str, to_path: &str) -> String {
    let a: Vec<&str> = from_folder.split('/').filter(|s| !s.is_empty()).collect();
    let b: Vec<&str> = to_path.split('/').filter(|s| !s.is_empty()).collect();
    let mut i = 0;
    while i < a.len() && i + 1 < b.len() && lookup_key(a[i]) == lookup_key(b[i]) {
        i += 1;
    }
    let mut out: Vec<&str> = std::iter::repeat_n("..", a.len() - i).collect();
    out.extend(&b[i..]);
    out.join("/")
}

/// Local redirects overlay for unconfirmed renames (§6.6, rule 8).
#[derive(Clone, Debug, Default)]
pub struct Redirects {
    ghosts: ResolveIndex,
}

impl Redirects {
    pub fn add(&mut self, old_path: &str, id: Id) {
        self.ghosts.insert(id, old_path);
    }
    pub fn remove(&mut self, id: Id) {
        self.ghosts.remove(id);
    }
    pub fn is_empty(&self) -> bool {
        self.ghosts.is_empty()
    }
    pub fn resolve(
        &self,
        ix: &ResolveIndex,
        target: &str,
        syntax: Syntax,
        folder: &str,
    ) -> Option<Resolved> {
        let r = ix.resolve(target, syntax, folder);
        if self.ghosts.is_empty() {
            return r;
        }
        match self.ghosts.resolve(target, syntax, folder) {
            Some(g) if ix.contains(&g.id) && r.map(|r| r.id) != Some(g.id) => Some(g),
            _ => r,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ix(paths: &[(&str, u8)]) -> ResolveIndex {
        let mut ix = ResolveIndex::new();
        for (p, i) in paths {
            ix.insert(Id([*i; 16]), p);
        }
        ix
    }
    fn r(ix: &ResolveIndex, t: &str, s: Syntax, f: &str) -> Option<u8> {
        ix.resolve(t, s, f).map(|r| r.id.0[0])
    }
    #[test]
    fn rules() {
        let x = ix(&[
            ("Note.md", 1),
            ("a/Note.md", 2),
            ("a/b/Deep.md", 3),
            ("img.PNG", 4),
            ("a/x y.md", 5),
        ]);
        assert_eq!(r(&x, "note", Syntax::Wiki, ""), Some(1));
        assert_eq!(
            r(&x, "Note", Syntax::Wiki, "a"),
            Some(2),
            "own folder first"
        );
        assert_eq!(r(&x, "a/Note", Syntax::Wiki, ""), Some(2));
        assert_eq!(r(&x, "b/Deep", Syntax::Wiki, ""), Some(3), "suffix");
        assert_eq!(r(&x, "../Note", Syntax::Wiki, "a/b"), Some(2));
        assert_eq!(r(&x, "img.png", Syntax::Wiki, ""), Some(4));
        assert_eq!(
            r(&x, "img", Syntax::Wiki, ""),
            None,
            "other extensions must be written"
        );
        assert_eq!(r(&x, "x y.md", Syntax::Markdown, "a"), Some(5));
        assert_eq!(r(&x, "https://x", Syntax::Markdown, ""), None);
        assert_eq!(r(&x, "../../..", Syntax::Wiki, ""), None);
        let y = ix(&[("Note 1.md", 1), ("note 1.md", 2)]);
        assert_eq!(
            r(&y, "note 1", Syntax::Wiki, ""),
            Some(2),
            "exact case wins among case-only duplicates"
        );
        assert_eq!(r(&y, "Note 1", Syntax::Wiki, ""), Some(1));
        assert_eq!(
            r(&y, "NOTE 1", Syntax::Wiki, ""),
            Some(1),
            "otherwise lexicographic"
        );
    }
    #[test]
    fn relative_paths() {
        assert_eq!(relative("a/b", "a/c/N.md"), "../c/N.md");
        assert_eq!(relative("", "a/N.md"), "a/N.md");
        assert_eq!(relative("a", "N.md"), "../N.md");
        assert_eq!(join("a/b", "../c"), Some("a/c".into()));
    }
}
