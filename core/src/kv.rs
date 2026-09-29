//! Client persistence is a single ordered key-value store (native: SQLite table `kv`;
//! web: one IndexedDB object store). State machines emit `Write`s that the host must commit
//! atomically, in order, before sending anything they produced in the same `Output`.

use crate::ids::{Hash, Id};

pub const P_META: u8 = b'm';
pub const P_ENTRY: u8 = b'e';
pub const P_PENDING: u8 = b'p';
pub const P_DOC: u8 = b'u';
pub const P_QUARANTINE: u8 = b'q';
pub const P_REDIRECT: u8 = b'r';
pub const P_BLOB: u8 = b'b';
pub const P_DOWNLOAD: u8 = b'y';

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    Put(Vec<u8>, Vec<u8>),
    Del(Vec<u8>),
}

pub fn meta_key(name: &str) -> Vec<u8> {
    let mut k = vec![P_META];
    k.extend_from_slice(name.as_bytes());
    k
}
pub fn entry_key(id: Id) -> Vec<u8> {
    let mut k = vec![P_ENTRY];
    k.extend_from_slice(&id.0);
    k
}
pub fn pending_key(op_id: u64) -> Vec<u8> {
    let mut k = vec![P_PENDING];
    k.extend_from_slice(&op_id.to_be_bytes());
    k
}
pub fn quarantine_key(op_id: u64) -> Vec<u8> {
    let mut k = vec![P_QUARANTINE];
    k.extend_from_slice(&op_id.to_be_bytes());
    k
}
pub fn redirect_key(op_id: u64, id: Id) -> Vec<u8> {
    let mut k = vec![P_REDIRECT];
    k.extend_from_slice(&op_id.to_be_bytes());
    k.extend_from_slice(&id.0);
    k
}
/// Prefix of all confirmed updates of one doc (scan it to read them in seq order).
pub fn doc_prefix(entry: Id, slot: &str) -> Vec<u8> {
    let mut k = vec![P_DOC];
    k.extend_from_slice(&entry.0);
    k.push(slot.len() as u8);
    k.extend_from_slice(slot.as_bytes());
    k
}
pub fn doc_key(entry: Id, slot: &str, seq: u64) -> Vec<u8> {
    let mut k = doc_prefix(entry, slot);
    k.extend_from_slice(&seq.to_be_bytes());
    k
}
pub fn parse_doc_key(k: &[u8]) -> Option<(Id, String, u64)> {
    if k.first() != Some(&P_DOC) || k.len() < 18 {
        return None;
    }
    let id = Id::from_slice(&k[1..17])?;
    let n = k[17] as usize;
    let slot = std::str::from_utf8(k.get(18..18 + n)?).ok()?.to_string();
    let seq = u64::from_be_bytes(k.get(18 + n..26 + n)?.try_into().ok()?);
    Some((id, slot, seq))
}
pub fn blob_key(h: Hash) -> Vec<u8> {
    let mut k = vec![P_BLOB];
    k.extend_from_slice(&h.0);
    k
}
pub fn download_key(h: Hash) -> Vec<u8> {
    let mut k = vec![P_DOWNLOAD];
    k.extend_from_slice(&h.0);
    k
}
