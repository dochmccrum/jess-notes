//! Rename/move link rewriting under Invariant R (DESIGN §6.6).

use crate::apply::RenameRec;
use crate::ids::Id;
use crate::links::{Link, Syntax};
use crate::names::lookup_key;
use crate::resolve::{join, relative, ResolveIndex, Resolved, Via};
use crate::state::MetaState;
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};

const MD_ENCODE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'%')
    .add(b'(')
    .add(b')')
    .add(b'<')
    .add(b'>')
    .add(b'#')
    .add(b'?');

/// A replacement of `[start16, end16)` of a doc's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rewrite {
    pub src: Id,
    pub start16: u32,
    pub end16: u32,
    pub old: String,
    pub new: String,
}

fn strip_md(p: &str) -> &str {
    if p.len() >= 3 && p[p.len() - 3..].eq_ignore_ascii_case(".md") {
        &p[..p.len() - 3]
    } else {
        p
    }
}

/// Formats a link target that resolves to `target` from `source_folder`, preserving the
/// original link's style. Returns the text for the link's target range.
pub fn format_target(
    ix: &ResolveIndex,
    target: Id,
    link: &Link,
    via: Via,
    source_folder: &str,
) -> Option<String> {
    let path = ix.path(&target)?;
    let is_md = path.len() >= 3 && path[path.len() - 3..].eq_ignore_ascii_case(".md");
    let wrote_ext = lookup_key(&link.target).ends_with(".md");
    let keep = |p: &str| -> String {
        if is_md && !wrote_ext {
            strip_md(p).to_string()
        } else {
            p.to_string()
        }
    };
    let base = path.rsplit('/').next().unwrap_or(path);
    let t = link.target.trim();
    let dot_prefix = t.starts_with("./");
    let slash_prefix = t.starts_with('/');
    let name_form = keep(base);
    let abs_form = if slash_prefix {
        format!("/{}", keep(path))
    } else {
        keep(path)
    };
    let mut rel = keep(&relative(source_folder, path));
    if dot_prefix && !rel.starts_with("../") {
        rel = format!("./{rel}");
    }
    let order: Vec<&String> = if slash_prefix {
        vec![&abs_form, &rel, &name_form]
    } else {
        match via {
            Via::Basename => vec![&name_form, &abs_form, &rel],
            Via::Absolute | Via::Suffix => vec![&abs_form, &name_form, &rel],
            Via::Relative => vec![&rel, &abs_form, &name_form],
        }
    };
    for cand in order {
        if ix.resolve(cand, link.syntax, source_folder).map(|r| r.id) == Some(target) {
            return Some(encode_for(link, cand));
        }
    }
    None
}

fn encode_for(link: &Link, s: &str) -> String {
    match link.syntax {
        Syntax::Wiki => s.to_string(),
        Syntax::Markdown if link.angle => s.to_string(),
        Syntax::Markdown => utf8_percent_encode(s, MD_ENCODE).to_string(),
    }
}

/// Text currently in the link's target range.
pub fn written_target(link: &Link) -> &str {
    match link.syntax {
        Syntax::Wiki => &link.target,
        Syntax::Markdown => &link.raw_target,
    }
}

/// Invariant R: for each candidate link `(src, link)` whose resolution changed between the
/// before and after states, produce a rewrite that restores the before-target.
pub fn invariant_r_rewrites<'a>(
    before: &MetaState,
    before_ix: &ResolveIndex,
    after: &MetaState,
    after_ix: &ResolveIndex,
    candidates: impl IntoIterator<Item = (Id, &'a Link)>,
) -> Vec<Rewrite> {
    let mut out = Vec::new();
    for (src, link) in candidates {
        let Some(rb) = before_ix.resolve(&link.target, link.syntax, &before.folder_of(src)) else {
            continue;
        };
        if !after_ix.contains(&rb.id) {
            continue; // target trashed/purged: exempt
        }
        let folder_after = after.folder_of(src);
        if after_ix
            .resolve(&link.target, link.syntax, &folder_after)
            .map(|r| r.id)
            == Some(rb.id)
        {
            continue;
        }
        if let Some(new) = format_target(after_ix, rb.id, link, rb.via, &folder_after) {
            let old = written_target(link).to_string();
            if new != old {
                out.push(Rewrite {
                    src,
                    start16: link.target_range.0,
                    end16: link.target_range.1,
                    old,
                    new,
                });
            }
        }
    }
    out
}

/// A `rename_history` row as seen by the stale-link rule.
#[derive(Clone, Debug)]
pub struct HistoryRow {
    pub seq: u64,
    pub rec: RenameRec,
    pub origin_replica: Option<u64>,
    pub origin_op: Option<u64>,
}

/// Stale-link rule (§6.6): a newly introduced link from a replica that hadn't seen some renames.
/// Returns the entry the link should point at, if exactly one rename explains it.
pub fn stale_link_target(
    ix: &ResolveIndex,
    link: &Link,
    source_folder: &str,
    history: &[HistoryRow],
    known_seq: u64,
    replica: u64,
    op_id: u64,
) -> Option<Resolved> {
    let mut found: Vec<Resolved> = Vec::new();
    let tl = lookup_key(link.target.trim());
    for h in history {
        if h.seq <= known_seq {
            continue;
        }
        if h.origin_replica == Some(replica)
            && h.origin_op.map(|o| o < op_id).unwrap_or(false)
            && !h.rec.induced
        {
            continue;
        }
        if h.rec.is_folder {
            // Path links into the old folder: map the remainder onto the new folder.
            let old = lookup_key(&h.rec.old_path);
            let joined =
                if link.syntax == Syntax::Markdown || tl.starts_with("./") || tl.starts_with("../")
                {
                    join(&lookup_key(source_folder), &tl).unwrap_or_default()
                } else {
                    tl.trim_start_matches('/').to_string()
                };
            for cand in [joined, tl.trim_start_matches('/').to_string()] {
                if let Some(rest) = cand.strip_prefix(&format!("{old}/")) {
                    let newp = format!("{}/{}", h.rec.new_path, rest);
                    if let Some(r) = ix.resolve(&newp, Syntax::Wiki, "") {
                        found.push(Resolved {
                            id: r.id,
                            via: Via::Absolute,
                        });
                        break;
                    }
                }
            }
            continue;
        }
        let mut ghost = ResolveIndex::new();
        ghost.insert(h.rec.entry, &h.rec.old_path);
        if let Some(g) = ghost.resolve(&link.target, link.syntax, source_folder) {
            if ix.contains(&g.id) {
                found.push(g);
            }
        }
    }
    found.sort_by_key(|r| r.id);
    found.dedup_by_key(|r| r.id);
    if found.len() != 1 {
        return None;
    }
    let r = found[0];
    if ix
        .resolve(&link.target, link.syntax, source_folder)
        .map(|x| x.id)
        == Some(r.id)
    {
        return None;
    }
    Some(r)
}

/// Applies rewrites to a string (UTF-16 ranges), last-first. Used by tests and non-yrs callers.
pub fn apply_to_string(text: &str, rewrites: &[Rewrite]) -> String {
    let mut u: Vec<u16> = text.encode_utf16().collect();
    let mut rs: Vec<&Rewrite> = rewrites.iter().collect();
    rs.sort_by_key(|r| std::cmp::Reverse(r.start16));
    for r in rs {
        let new: Vec<u16> = r.new.encode_utf16().collect();
        u.splice(r.start16 as usize..r.end16 as usize, new);
    }
    String::from_utf16_lossy(&u)
}
