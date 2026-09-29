//! Vault → folder layout (DESIGN §12.1). Shared by export, the server mirror, integrity-check
//! and the tests. `Exact` is an identity mapping; `Portable` sanitises names for other
//! filesystems (Windows, Android/iOS shared storage) deterministically and reports every change.

use crate::ids::{Hash, Id};
use crate::model::{Entry, KIND_FOLDER, KIND_MARKDOWN, KIND_VAULT};
use crate::names::split_ext;
use crate::state::MetaState;
use std::collections::{BTreeMap, HashMap};
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Exact,
    Portable,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub profile: Profile,
    /// Include trashed entries under `.trash/`.
    pub include_trash: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            profile: Profile::Exact,
            include_trash: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Dir,
    /// Markdown text from the entry's `body` doc.
    Text(Id),
    Blob(Hash),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: Id,
    pub path: String,
    pub source: Source,
    pub modified_at: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Projection {
    /// Sorted by path; a directory precedes its contents.
    pub items: Vec<Item>,
    /// (vault path, projected path) for every name the profile changed.
    pub mapped: Vec<(String, String)>,
    /// Entries that could not be projected (reported, never silently dropped).
    pub errors: Vec<String>,
}

impl Projection {
    /// `EXPORT-REPORT.txt` contents (portable profile mappings and errors).
    pub fn report(&self) -> String {
        let mut s = String::from("Jess export report\n\n");
        if self.mapped.is_empty() && self.errors.is_empty() {
            s.push_str("All names were exported unchanged.\n");
        }
        if !self.mapped.is_empty() {
            s.push_str("Renamed for portability:\n");
            for (a, b) in &self.mapped {
                s.push_str(&format!("  {a}  ->  {b}\n"));
            }
        }
        if !self.errors.is_empty() {
            s.push_str("\nNot exported:\n");
            for e in &self.errors {
                s.push_str(&format!("  {e}\n"));
            }
        }
        s
    }
    pub fn by_id(&self) -> HashMap<Id, &Item> {
        self.items.iter().map(|i| (i.id, i)).collect()
    }
}

const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Portable form of one path segment (before collision handling).
pub fn sanitize_segment(name: &str) -> String {
    let mut s: String = name
        .nfc()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
                || (c as u32) < 0x20
                || c == '\u{7f}'
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    if s.is_empty() {
        s.push('_');
    }
    let stem_end = s.find('.').unwrap_or(s.len());
    if RESERVED.contains(&s[..stem_end].to_lowercase().as_str()) {
        s.insert(stem_end, '_');
    }
    s
}

fn with_paren_suffix(name: &str, n: u32, is_folder: bool) -> String {
    let (stem, ext) = split_ext(name, is_folder);
    format!("{stem} ({n}){ext}")
}

fn source_of(e: &Entry) -> Option<Source> {
    if e.kind == KIND_FOLDER {
        return Some(Source::Dir);
    }
    match (e.kind.as_str(), e.blob) {
        (_, Some(h)) => Some(Source::Blob(h)),
        (KIND_MARKDOWN, None) => Some(Source::Text(e.id)),
        _ => None,
    }
}

/// Projects the vault. Live entries only unless `include_trash`.
pub fn project(st: &MetaState, opts: Options) -> Projection {
    let mut out = Projection::default();
    let include = |e: &Entry| {
        !e.purged && e.kind != KIND_VAULT && (e.trashed.is_none() || opts.include_trash)
    };
    // Children by parent, sorted by id for deterministic collision handling.
    let mut kids: BTreeMap<Option<Id>, Vec<&Entry>> = BTreeMap::new();
    for e in st.iter().filter(|e| include(e)) {
        let parent = e.parent.filter(|p| st.get(p).map(include).unwrap_or(false));
        kids.entry(parent).or_default().push(e);
    }
    for v in kids.values_mut() {
        v.sort_by_key(|e| e.id);
    }
    // Walk top-down: (parent id, projected folder path, vault folder path, in trash).
    let mut stack: Vec<(Option<Id>, String, String)> = vec![(None, String::new(), String::new())];
    while let Some((pid, ppath, vpath)) = stack.pop() {
        let Some(children) = kids.get(&pid) else {
            continue;
        };
        let mut taken: HashMap<String, u32> = HashMap::new();
        let mut assigned: Vec<(&Entry, String)> = Vec::new();
        for e in children {
            let trashed_root = pid.is_none() && e.trashed.is_some();
            let base = match opts.profile {
                Profile::Exact => e.name.clone(),
                Profile::Portable => sanitize_segment(&e.name),
            };
            let key = |s: &str| match opts.profile {
                Profile::Exact => crate::names::name_key(s),
                Profile::Portable => s.to_lowercase(),
            };
            // Trashed roots live in `.trash/`, keyed separately.
            let scope = if trashed_root { ".trash/" } else { "" };
            let mut name = base.clone();
            let mut n = 1;
            while taken.contains_key(&format!("{scope}{}", key(&name))) {
                n += 1;
                name = with_paren_suffix(&base, n, e.is_folder());
            }
            taken.insert(format!("{scope}{}", key(&name)), 1);
            assigned.push((e, name));
        }
        for (e, name) in assigned {
            let trashed_root = pid.is_none() && e.trashed.is_some();
            let prefix = if trashed_root {
                ".trash/".to_string()
            } else if ppath.is_empty() {
                String::new()
            } else {
                format!("{ppath}/")
            };
            let vprefix = if vpath.is_empty() {
                String::new()
            } else {
                format!("{vpath}/")
            };
            let path = format!("{prefix}{name}");
            let vp = format!("{vprefix}{}", e.name);
            if name != e.name {
                out.mapped.push((vp.clone(), path.clone()));
            }
            match source_of(e) {
                Some(src) => {
                    let is_dir = src == Source::Dir;
                    out.items.push(Item {
                        id: e.id,
                        path: path.clone(),
                        source: src,
                        modified_at: e.modified_at,
                    });
                    if is_dir {
                        stack.push((Some(e.id), path, vp));
                    }
                }
                None => out
                    .errors
                    .push(format!("{vp}: {} entry without content", e.kind)),
            }
        }
    }
    out.items.sort_by(|a, b| a.path.cmp(&b.path));
    out.mapped.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::{apply_meta, Ctx, Mode, Tx};
    use crate::hlc::Hlc;
    use crate::ops::MetaOp;

    fn mk(st: &mut MetaState, i: u8, parent: Option<u8>, name: &str, folder: bool) {
        let op = MetaOp::Create {
            id: Id([i; 16]),
            kind: if folder { "folder" } else { "markdown" }.into(),
            parent: parent.map(|p| Id([p; 16])),
            name: name.into(),
            tree_visible: true,
            blob: None,
            blob_info: None,
            created_at: None,
            modified_at: None,
            props: vec![],
        };
        apply_meta(
            st,
            &mut Tx::new(),
            &op,
            &Ctx {
                hlc: Hlc::new(i as u64, 0, 1),
                known_seq: 0,
                replica: 1,
                op_id: i as u64,
                mode: Mode::Server,
                now: 0,
            },
        )
        .unwrap();
    }

    #[test]
    fn exact_and_portable() {
        let mut st = MetaState::new();
        mk(&mut st, 1, None, "F:ol", true);
        mk(&mut st, 2, Some(1), "a.md", false);
        mk(&mut st, 3, Some(1), "A.md", false);
        mk(&mut st, 4, None, "CON.md", false);
        let e = project(&st, Options::default());
        let paths: Vec<&str> = e.items.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, vec!["CON.md", "F:ol", "F:ol/A.md", "F:ol/a.md"]);
        let p = project(
            &st,
            Options {
                profile: Profile::Portable,
                include_trash: false,
            },
        );
        let paths: Vec<&str> = p.items.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, vec!["CON_.md", "F_ol", "F_ol/A (2).md", "F_ol/a.md"]);
        assert_eq!(p.mapped.len(), 3);
    }
}
