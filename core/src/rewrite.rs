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
    /// The entry the rewritten link must resolve to (checked by [`keeps_parse`]).
    pub target: Option<Id>,
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
                    target: Some(rb.id),
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

/// Whether `rewrites` keep the doc's links intact (§6.6): rewriting a link's target can change
/// how the text around it parses (`[t]( [[Old name]]e.md)` isn't a markdown link, because of the
/// space, but `[t]( [[New]]e.md)` is, and swallows the wikilink). After the rewrites, the text must
/// have the same links in the same order, each rewritten one resolving to its `target` and every
/// other one to what `resolve` gives it now.
pub fn keeps_parse(
    text: &str,
    rewrites: &[Rewrite],
    resolve: impl Fn(&Link) -> Option<Id>,
) -> bool {
    let before = crate::links::extract(text).links;
    let after = crate::links::extract(&apply_to_string(text, rewrites)).links;
    before.len() == after.len()
        && before.iter().zip(&after).all(|(b, a)| {
            let expect = match rewrites
                .iter()
                .find(|r| (r.start16, r.end16) == b.target_range)
            {
                Some(r) => r.target,
                None => resolve(b),
            };
            resolve(a) == expect
        })
}

/// The alternative spelling [`keeps_parse`] falls back on: a wikilink without alias or subpath
/// keeps its old text as its alias (`[[Old name]]` → `[[New|Old name]]`), so it reads the same and
/// the characters around it stay as they were.
pub fn with_alias(link: &Link, rw: &Rewrite) -> Option<Rewrite> {
    (link.syntax == Syntax::Wiki && link.display.is_none() && link.subpath.is_none()).then(|| {
        Rewrite {
            new: format!("{}|{}", rw.new, link.target),
            ..rw.clone()
        }
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::extract;

    fn id(n: u8) -> Id {
        Id::new_v7(1, [n; 10])
    }

    /// A rewrite of the link at index `i` of `text` to `new`, meant to resolve to `target`.
    fn rw(text: &str, i: usize, new: &str, target: Id) -> (Link, Rewrite) {
        let l = extract(text).links[i].clone();
        let r = Rewrite {
            src: id(0),
            start16: l.target_range.0,
            end16: l.target_range.1,
            old: written_target(&l).into(),
            new: new.into(),
            target: Some(target),
        };
        (l, r)
    }

    #[test]
    fn a_rewrite_that_changes_the_surrounding_parse_is_caught_and_an_alias_fixes_it() {
        // Simulation seed 280719: the space in the old name keeps `[t]( … )` from being a
        // markdown link; without it, the markdown link swallows the wikilink.
        let text = " [[x y]]  [t](Note [t]( [[Pasted image.png]]e.md) .md) ";
        let img = id(1);
        let resolve = |l: &Link| match l.target.as_str() {
            "Ünï" => Some(img),
            "x y" => Some(id(2)),
            _ => None,
        };
        let (l, plain) = rw(text, 1, "Ünï", img);
        assert!(!keeps_parse(text, std::slice::from_ref(&plain), resolve));
        let alias = with_alias(&l, &plain).unwrap();
        assert_eq!(alias.new, "Ünï|Pasted image.png");
        assert!(keeps_parse(text, &[alias], resolve));
    }

    #[test]
    fn ordinary_rewrites_keep_the_parse() {
        let text = "See [[Old]] and [t](Old.md).";
        let new = id(1);
        let resolve = |l: &Link| (l.target == "New" || l.target == "New.md").then_some(new);
        let (_, a) = rw(text, 0, "New", new);
        let (_, b) = rw(text, 1, "New.md", new);
        assert!(keeps_parse(text, &[a, b], resolve));
    }

    #[test]
    fn no_alias_for_links_that_have_one_or_a_subpath() {
        for text in ["[[Old|shown]]", "[[Old#Heading]]", "[t](Old.md)"] {
            let (l, r) = rw(text, 0, "New", id(1));
            assert!(with_alias(&l, &r).is_none(), "{text}");
        }
    }
}
