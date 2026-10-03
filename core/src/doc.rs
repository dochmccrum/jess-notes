//! yrs helpers (feature `yrs`): note text is a Yjs doc with one `Y.Text` named `t` (§3.1).
//! Offsets are UTF-16 (`OffsetKind::Utf16`) to match Yjs.

use crate::rewrite::Rewrite;
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, GetString, OffsetKind, Options, ReadTxn, StateVector, Text, Transact, Update};

pub const TEXT_NAME: &str = "t";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadUpdate;

pub fn new_doc(client_id: u64) -> Doc {
    let mut o = Options::with_client_id(yrs::block::ClientID::new(client_id & ((1 << 53) - 1)));
    o.offset_kind = OffsetKind::Utf16;
    o.skip_gc = false;
    Doc::with_options(o)
}

pub fn decode(bytes: &[u8]) -> Result<Update, BadUpdate> {
    Update::decode_v1(bytes).map_err(|_| BadUpdate)
}

pub fn apply(doc: &Doc, bytes: &[u8]) -> Result<(), BadUpdate> {
    let u = decode(bytes)?;
    let mut txn = doc.transact_mut();
    txn.apply_update(u).map_err(|_| BadUpdate)
}

pub fn text(doc: &Doc) -> String {
    let t = doc.get_or_insert_text(TEXT_NAME);
    let txn = doc.transact();
    t.get_string(&txn)
}

pub fn state_vector(doc: &Doc) -> Vec<u8> {
    doc.transact().state_vector().encode_v1()
}

pub fn encode_state(doc: &Doc) -> Vec<u8> {
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
}

pub fn diff(doc: &Doc, sv: &[u8]) -> Result<Vec<u8>, BadUpdate> {
    let sv = StateVector::decode_v1(sv).map_err(|_| BadUpdate)?;
    Ok(doc.transact().encode_diff_v1(&sv))
}

/// Does `update` carry insertions not contained in `state` (another update, e.g. a purged doc's
/// merged rows)? Exact ID-set containment, so pending structs with clock gaps are handled
/// (recovered-after-purge, §6.4).
pub fn has_new_content(update: &[u8], state: &[u8]) -> Result<bool, BadUpdate> {
    let u = decode(update)?.insertions(true);
    let have = if state.is_empty() {
        yrs::IdSet::new()
    } else {
        decode(state)?.insertions(true)
    };
    for (client, ranges) in u.iter() {
        let theirs: Vec<std::ops::Range<u32>> = have
            .iter()
            .find(|(c, _)| *c == client)
            .map(|(_, r)| r.iter().cloned().collect())
            .unwrap_or_default();
        for r in ranges.iter() {
            let mut pos = r.start;
            while pos < r.end {
                match theirs
                    .iter()
                    .filter(|t| t.start <= pos && t.end > pos)
                    .map(|t| t.end)
                    .max()
                {
                    Some(e) => pos = e,
                    None => return Ok(true),
                }
            }
        }
    }
    Ok(false)
}

/// Does the doc hold updates it couldn't integrate (missing dependencies)?
pub fn has_pending(doc: &Doc) -> bool {
    let txn = doc.transact();
    txn.store().pending_update().is_some() || txn.store().pending_ds().is_some()
}

/// Merges updates into one (Y.mergeUpdates).
pub fn merge(updates: &[Vec<u8>]) -> Result<Vec<u8>, BadUpdate> {
    yrs::merge_updates_v1(updates.iter().map(|u| u.as_slice())).map_err(|_| BadUpdate)
}

/// Replaces `[start, end)` ranges (UTF-16) in the doc text and returns the resulting update.
pub fn apply_rewrites(doc: &Doc, rewrites: &[Rewrite]) -> Vec<u8> {
    let t = doc.get_or_insert_text(TEXT_NAME);
    let sv = doc.transact().state_vector();
    {
        let mut txn = doc.transact_mut();
        let mut rs: Vec<&Rewrite> = rewrites.iter().collect();
        rs.sort_by_key(|r| std::cmp::Reverse(r.start16));
        let len = t.len(&txn);
        for r in rs {
            if r.end16 > len || r.start16 > r.end16 {
                continue;
            }
            t.remove_range(&mut txn, r.start16, r.end16 - r.start16);
            t.insert(&mut txn, r.start16, &r.new);
        }
    }
    doc.transact().encode_diff_v1(&sv)
}

/// Replaces the whole text via a minimal diff (common prefix/suffix), returning the update.
pub fn set_text(doc: &Doc, new: &str) -> Vec<u8> {
    let old = text(doc);
    let a: Vec<u16> = old.encode_utf16().collect();
    let b: Vec<u16> = new.encode_utf16().collect();
    let mut p = 0;
    while p < a.len() && p < b.len() && a[p] == b[p] {
        p += 1;
    }
    // Don't split a surrogate pair.
    while p > 0 && (0xDC00..0xE000).contains(&a.get(p).copied().unwrap_or(0)) {
        p -= 1;
    }
    let mut s = 0;
    while s < a.len() - p && s < b.len() - p && a[a.len() - 1 - s] == b[b.len() - 1 - s] {
        s += 1;
    }
    while s > 0 && (0xDC00..0xE000).contains(&a[a.len() - s]) {
        s -= 1;
    }
    let ins = String::from_utf16_lossy(&b[p..b.len() - s]);
    let r = Rewrite {
        src: crate::Id::default(),
        start16: p as u32,
        end16: (a.len() - s) as u32,
        old: String::new(),
        new: ins,
        target: None,
    };
    apply_rewrites(doc, &[r])
}

/// Inserts text at a UTF-16 offset, returning the update.
pub fn insert(doc: &Doc, at16: u32, s: &str) -> Vec<u8> {
    let t = doc.get_or_insert_text(TEXT_NAME);
    let sv = doc.transact().state_vector();
    {
        let mut txn = doc.transact_mut();
        let at = at16.min(t.len(&txn));
        t.insert(&mut txn, at, s);
    }
    doc.transact().encode_diff_v1(&sv)
}

/// Removes a UTF-16 range, returning the update.
pub fn remove(doc: &Doc, at16: u32, len16: u32) -> Vec<u8> {
    let t = doc.get_or_insert_text(TEXT_NAME);
    let sv = doc.transact().state_vector();
    {
        let mut txn = doc.transact_mut();
        let l = t.len(&txn);
        let at = at16.min(l);
        let n = len16.min(l - at);
        if n > 0 {
            t.remove_range(&mut txn, at, n);
        }
    }
    doc.transact().encode_diff_v1(&sv)
}

pub fn len16(doc: &Doc) -> u32 {
    let t = doc.get_or_insert_text(TEXT_NAME);
    let txn = doc.transact();
    t.len(&txn)
}

/// Builds a doc from a list of updates.
pub fn from_updates(client_id: u64, updates: &[Vec<u8>]) -> Result<Doc, BadUpdate> {
    let d = new_doc(client_id);
    for u in updates {
        apply(&d, u)?;
    }
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf16_edits_and_merge() {
        let a = new_doc(1);
        let u1 = insert(&a, 0, "😀 [[Old]]\r\n");
        let b = new_doc(2);
        apply(&b, &u1).unwrap();
        let u2 = apply_rewrites(
            &b,
            &[Rewrite {
                src: crate::Id::default(),
                start16: 5,
                end16: 8,
                old: "Old".into(),
                new: "New".into(),
                target: None,
            }],
        );
        apply(&a, &u2).unwrap();
        assert_eq!(text(&a), "😀 [[New]]\r\n");
        let m = merge(&[u1.clone(), u2]).unwrap();
        assert_eq!(text(&from_updates(3, &[m]).unwrap()), "😀 [[New]]\r\n");
        let st = encode_state(&a);
        assert!(!has_new_content(&u1, &st).unwrap());
        let c = new_doc(9);
        let u3 = insert(&c, 0, "x");
        assert!(has_new_content(&u3, &st).unwrap());
        assert!(has_new_content(&u3, &[]).unwrap());
        let u4 = set_text(&a, "😀 [[Newer]]\r\n");
        apply(&b, &u4).unwrap();
        assert_eq!(text(&b), "😀 [[Newer]]\r\n");
        assert!(decode(b"\xff\xff").is_err() || apply(&b, b"\xff\xff").is_err());
    }
}
