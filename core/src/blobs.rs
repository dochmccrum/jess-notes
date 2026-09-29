//! Sans-IO blob transfer state machine (DESIGN §7): persisted upload queue, resumable chunked
//! uploads, prioritised ranged downloads, and the eviction rule (never evict before the server
//! confirms the blob).

use crate::ids::Hash;
use crate::kv::{self, Write};
use minicbor::{Decode, Encode};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const CHUNK_SIZE: u64 = 4 << 20;
pub const MAX_UPLOADS: usize = 2;
pub const CHUNKS_PER_UPLOAD: usize = 2;
pub const MAX_DOWNLOAD_RANGES: usize = 4;

/// Download priorities (§7.4). Lower is more urgent.
pub const P0_OPEN: u8 = 0;
pub const P1_EMBED: u8 = 1;
pub const P2_RECENT: u8 = 2;
pub const P3_PREFETCH: u8 = 3;
pub const P4_DERIVED: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum LocalState {
    /// Only on this device: must be uploaded; can never be evicted.
    #[n(0)]
    LocalOnly,
    #[n(1)]
    Uploading,
    /// The server has it (`present`). Evictable if not pinned.
    #[n(2)]
    Confirmed,
    /// Known remote blob being (partially) downloaded.
    #[n(3)]
    Remote,
}

/// Bitmap of chunk indexes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Encode, Decode)]
#[cbor(transparent)]
pub struct Bitmap(#[cbor(n(0), with = "minicbor::bytes")] pub Vec<u8>);

impl Bitmap {
    pub fn full(n: u32) -> Bitmap {
        let mut b = Bitmap::default();
        for i in 0..n {
            b.set(i);
        }
        b
    }
    pub fn get(&self, i: u32) -> bool {
        self.0
            .get((i / 8) as usize)
            .map(|b| b & (1 << (i % 8)) != 0)
            .unwrap_or(false)
    }
    pub fn set(&mut self, i: u32) {
        let byte = (i / 8) as usize;
        if self.0.len() <= byte {
            self.0.resize(byte + 1, 0);
        }
        self.0[byte] |= 1 << (i % 8);
    }
    pub fn count(&self, n: u32) -> u32 {
        (0..n).filter(|i| self.get(*i)).count() as u32
    }
    pub fn is_full(&self, n: u32) -> bool {
        (0..n).all(|i| self.get(i))
    }
}

pub fn chunk_count(size: u64, chunk: u64) -> u32 {
    size.div_ceil(chunk).max(1) as u32
}

#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct LocalBlob {
    #[n(0)]
    pub hash: Hash,
    #[n(1)]
    pub size: u64,
    #[n(2)]
    pub state: LocalState,
    /// Chunks present locally.
    #[n(3)]
    pub have: Bitmap,
    #[n(4)]
    pub last_access: u64,
    #[n(5)]
    pub pinned: bool,
    /// Server upload session (resumable across restarts).
    #[n(6)]
    pub upload_id: Option<String>,
    /// Chunks the server has confirmed for the upload session.
    #[n(7)]
    pub uploaded: Bitmap,
}

impl LocalBlob {
    pub fn complete_locally(&self, chunk: u64) -> bool {
        self.have.is_full(chunk_count(self.size, chunk))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlobTask {
    /// `POST /api/blobs/{hash}/uploads {size}`
    Begin { hash: Hash, size: u64 },
    /// `PUT …/chunks/{index}` with bytes `[offset, offset+len)` of the local file.
    PutChunk {
        hash: Hash,
        upload_id: String,
        index: u32,
        offset: u64,
        len: u64,
    },
    /// `POST …/complete`
    Complete { hash: Hash, upload_id: String },
    /// `GET /api/blobs/{hash}` with `Range: bytes=offset-(offset+len-1)`; write into local storage.
    GetRange {
        hash: Hash,
        index: u32,
        offset: u64,
        len: u64,
    },
    /// `POST /api/blobs/presence`
    Presence { hashes: Vec<Hash> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlobResult {
    Began {
        hash: Hash,
        present: bool,
        upload_id: Option<String>,
        received: Bitmap,
    },
    ChunkDone {
        hash: Hash,
        index: u32,
        ok: bool,
    },
    Completed {
        hash: Hash,
        ok: bool,
    },
    /// The server no longer knows the upload session (expired): start over.
    UploadGone {
        hash: Hash,
    },
    RangeDone {
        hash: Hash,
        index: u32,
        ok: bool,
    },
    Presence {
        present: Vec<Hash>,
        missing: Vec<Hash>,
    },
    /// Transport failure for a task: it becomes eligible again.
    Failed {
        task: BlobTask,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlobProgress {
    pub uploads_pending: usize,
    pub uploads_total: usize,
    pub downloads_pending: usize,
}

#[derive(Debug, Default)]
pub struct BlobManager {
    pub chunk: u64,
    pub local: HashMap<Hash, LocalBlob>,
    /// hash → priority
    downloads: HashMap<Hash, (u8, u64)>,
    in_flight_begin: BTreeSet<Hash>,
    in_flight_chunks: BTreeMap<Hash, BTreeSet<u32>>,
    in_flight_complete: BTreeSet<Hash>,
    in_flight_ranges: BTreeMap<Hash, BTreeSet<u32>>,
    presence_in_flight: bool,
    uploads_done_session: usize,
}

impl BlobManager {
    pub fn new(chunk: u64) -> BlobManager {
        BlobManager {
            chunk,
            ..Default::default()
        }
    }

    fn put(&self, b: &LocalBlob) -> Write {
        Write::Put(kv::blob_key(b.hash), minicbor::to_vec(b).expect("encode"))
    }

    pub fn load_blob(&mut self, v: &[u8]) {
        if let Ok(b) = minicbor::decode::<LocalBlob>(v) {
            self.local.insert(b.hash, b);
        }
    }
    pub fn load_download(&mut self, k: &[u8], v: &[u8]) {
        if let (Some(h), Ok((p, s))) = (Hash::from_slice(&k[1..]), minicbor::decode::<(u8, u64)>(v))
        {
            self.downloads.insert(h, (p, s));
        }
    }

    /// A file was ingested into local storage (bytes fully present locally).
    pub fn ingest(&mut self, hash: Hash, size: u64, now: u64) -> Vec<Write> {
        let n = chunk_count(size, self.chunk);
        let b = self.local.entry(hash).or_insert_with(|| LocalBlob {
            hash,
            size,
            state: LocalState::LocalOnly,
            have: Bitmap::default(),
            last_access: now,
            pinned: false,
            upload_id: None,
            uploaded: Bitmap::default(),
        });
        b.have = Bitmap::full(n);
        b.last_access = now;
        // A new reference: re-verify with the server even if it once had the bytes (it may have
        // garbage-collected them since). `Begin` answers "present" cheaply if it still has them.
        if b.state == LocalState::Remote || b.state == LocalState::Confirmed {
            b.state = LocalState::LocalOnly;
            b.upload_id = None;
            b.uploaded = Bitmap::default();
        }
        let b = b.clone();
        let mut w = vec![self.put(&b)];
        if self.downloads.remove(&hash).is_some() {
            w.push(Write::Del(kv::download_key(hash)));
        }
        w
    }

    /// The server reports the blob as present (Changes blob row, upload complete, presence).
    pub fn server_present(&mut self, hash: Hash) -> Vec<Write> {
        match self.local.get_mut(&hash) {
            Some(b) if b.state != LocalState::Confirmed && b.state != LocalState::Remote => {
                b.state = LocalState::Confirmed;
                b.upload_id = None;
                let b = b.clone();
                self.uploads_done_session += 1;
                vec![self.put(&b)]
            }
            _ => vec![],
        }
    }

    /// The server reports the blob as not present (e.g. GC'd while unreferenced). A confirmed
    /// local copy goes back to the upload queue so it can never be lost if referenced again.
    pub fn server_absent(&mut self, hash: Hash) -> Vec<Write> {
        let chunk = self.chunk;
        match self.local.get_mut(&hash) {
            Some(b) if b.state == LocalState::Confirmed => {
                b.state = if b.complete_locally(chunk) {
                    LocalState::LocalOnly
                } else {
                    LocalState::Remote
                };
                b.upload_id = None;
                b.uploaded = Bitmap::default();
                let b = b.clone();
                vec![self.put(&b)]
            }
            _ => vec![],
        }
    }

    /// Ask for a blob's bytes at a priority (re-ranks if already queued).
    pub fn want(&mut self, hash: Hash, size: u64, prio: u8, now: u64) -> Vec<Write> {
        if let Some(b) = self.local.get_mut(&hash) {
            b.last_access = now;
            if b.complete_locally(self.chunk) {
                return vec![];
            }
        }
        let cur = self.downloads.get(&hash).map(|x| x.0);
        if cur.map(|c| c <= prio).unwrap_or(false) {
            return vec![];
        }
        self.downloads.insert(hash, (prio, size));
        self.local.entry(hash).or_insert_with(|| LocalBlob {
            hash,
            size,
            state: LocalState::Remote,
            have: Bitmap::default(),
            last_access: now,
            pinned: false,
            upload_id: None,
            uploaded: Bitmap::default(),
        });
        let b = self.local[&hash].clone();
        vec![
            Write::Put(
                kv::download_key(hash),
                minicbor::to_vec((prio, size)).expect("encode"),
            ),
            self.put(&b),
        ]
    }

    pub fn is_local(&self, hash: &Hash) -> bool {
        self.local
            .get(hash)
            .map(|b| b.complete_locally(self.chunk))
            .unwrap_or(false)
    }

    /// Hashes that must still be uploaded.
    pub fn unconfirmed(&self) -> Vec<Hash> {
        let mut v: Vec<Hash> = self
            .local
            .values()
            .filter(|b| matches!(b.state, LocalState::LocalOnly | LocalState::Uploading))
            .map(|b| b.hash)
            .collect();
        v.sort();
        v
    }

    pub fn progress(&self) -> BlobProgress {
        let pending = self.unconfirmed().len();
        BlobProgress {
            uploads_pending: pending,
            uploads_total: pending + self.uploads_done_session,
            downloads_pending: self.downloads.len(),
        }
    }

    /// Reconciliation (§7.6): ask the server which local-only blobs it has.
    pub fn presence_task(&mut self) -> Option<BlobTask> {
        if self.presence_in_flight {
            return None;
        }
        let hashes: Vec<Hash> = self
            .local
            .values()
            .filter(|b| b.state == LocalState::LocalOnly)
            .map(|b| b.hash)
            .collect();
        if hashes.is_empty() {
            return None;
        }
        self.presence_in_flight = true;
        Some(BlobTask::Presence { hashes })
    }

    /// Transport reset (disconnect / restart): forget in-flight work; everything is retried.
    pub fn reset_in_flight(&mut self) {
        self.in_flight_begin.clear();
        self.in_flight_chunks.clear();
        self.in_flight_complete.clear();
        self.in_flight_ranges.clear();
        self.presence_in_flight = false;
    }

    /// Next transfer tasks within the concurrency limits.
    pub fn next_tasks(&mut self) -> Vec<BlobTask> {
        let mut out = Vec::new();
        // Uploads: up to MAX_UPLOADS sessions, CHUNKS_PER_UPLOAD chunks each.
        let mut active: Vec<Hash> = self
            .local
            .values()
            .filter(|b| {
                matches!(b.state, LocalState::LocalOnly | LocalState::Uploading)
                    && b.complete_locally(self.chunk)
            })
            .map(|b| b.hash)
            .collect();
        active.sort();
        let mut sessions = 0;
        for h in active {
            if sessions >= MAX_UPLOADS {
                break;
            }
            sessions += 1;
            if self.in_flight_begin.contains(&h) || self.in_flight_complete.contains(&h) {
                continue;
            }
            let b = &self.local[&h];
            let n = chunk_count(b.size, self.chunk);
            match &b.upload_id {
                None => {
                    self.in_flight_begin.insert(h);
                    out.push(BlobTask::Begin {
                        hash: h,
                        size: b.size,
                    });
                }
                Some(uid) if b.uploaded.is_full(n) => {
                    if self
                        .in_flight_chunks
                        .get(&h)
                        .map(|s| s.is_empty())
                        .unwrap_or(true)
                    {
                        self.in_flight_complete.insert(h);
                        out.push(BlobTask::Complete {
                            hash: h,
                            upload_id: uid.clone(),
                        });
                    }
                }
                Some(uid) => {
                    let uid = uid.clone();
                    let (size, uploaded) = (b.size, b.uploaded.clone());
                    let fl = self.in_flight_chunks.entry(h).or_default();
                    for i in 0..n {
                        if fl.len() >= CHUNKS_PER_UPLOAD {
                            break;
                        }
                        if uploaded.get(i) || fl.contains(&i) {
                            continue;
                        }
                        fl.insert(i);
                        let offset = i as u64 * self.chunk;
                        out.push(BlobTask::PutChunk {
                            hash: h,
                            upload_id: uid.clone(),
                            index: i,
                            offset,
                            len: (size - offset).min(self.chunk),
                        });
                    }
                }
            }
        }
        // Downloads by priority, then hash for determinism.
        let mut q: Vec<(u8, Hash, u64)> = self
            .downloads
            .iter()
            .map(|(h, (p, s))| (*p, *h, *s))
            .collect();
        q.sort();
        let mut in_flight: usize = self.in_flight_ranges.values().map(|s| s.len()).sum();
        for (_, h, size) in q {
            if in_flight >= MAX_DOWNLOAD_RANGES {
                break;
            }
            let n = chunk_count(size, self.chunk);
            let have = self
                .local
                .get(&h)
                .map(|b| b.have.clone())
                .unwrap_or_default();
            let fl = self.in_flight_ranges.entry(h).or_default();
            for i in 0..n {
                if in_flight >= MAX_DOWNLOAD_RANGES {
                    break;
                }
                if have.get(i) || fl.contains(&i) {
                    continue;
                }
                fl.insert(i);
                in_flight += 1;
                let offset = i as u64 * self.chunk;
                out.push(BlobTask::GetRange {
                    hash: h,
                    index: i,
                    offset,
                    len: (size - offset).min(self.chunk),
                });
            }
        }
        out
    }

    pub fn on_result(&mut self, r: BlobResult, now: u64) -> Vec<Write> {
        let mut w = Vec::new();
        match r {
            BlobResult::Began {
                hash,
                present,
                upload_id,
                received,
            } => {
                self.in_flight_begin.remove(&hash);
                if present {
                    w.extend(self.server_present(hash));
                } else if let Some(b) = self.local.get_mut(&hash) {
                    if b.state == LocalState::Confirmed {
                        return w;
                    }
                    b.state = LocalState::Uploading;
                    b.upload_id = upload_id;
                    b.uploaded = received;
                    let b = b.clone();
                    w.push(self.put(&b));
                }
            }
            BlobResult::ChunkDone { hash, index, ok } => {
                if let Some(s) = self.in_flight_chunks.get_mut(&hash) {
                    s.remove(&index);
                }
                if ok {
                    if let Some(b) = self.local.get_mut(&hash) {
                        if !b.uploaded.get(index) {
                            b.uploaded.set(index);
                            let b = b.clone();
                            w.push(self.put(&b));
                        }
                    }
                }
            }
            BlobResult::Completed { hash, ok } => {
                self.in_flight_complete.remove(&hash);
                if ok {
                    w.extend(self.server_present(hash));
                } else if let Some(b) = self.local.get_mut(&hash) {
                    // Hash mismatch: restart the session (host re-hashes the source).
                    b.upload_id = None;
                    b.uploaded = Bitmap::default();
                    let b = b.clone();
                    w.push(self.put(&b));
                }
            }
            BlobResult::UploadGone { hash } => {
                self.in_flight_chunks.remove(&hash);
                self.in_flight_complete.remove(&hash);
                if let Some(b) = self.local.get_mut(&hash) {
                    if b.state != LocalState::Confirmed {
                        b.upload_id = None;
                        b.uploaded = Bitmap::default();
                        let b = b.clone();
                        w.push(self.put(&b));
                    }
                }
            }
            BlobResult::RangeDone { hash, index, ok } => {
                if let Some(s) = self.in_flight_ranges.get_mut(&hash) {
                    s.remove(&index);
                }
                if ok {
                    if let Some(b) = self.local.get_mut(&hash) {
                        b.have.set(index);
                        b.last_access = now;
                        let done = b.complete_locally(self.chunk);
                        if done && b.state == LocalState::Remote {
                            b.state = LocalState::Confirmed;
                        }
                        let b = b.clone();
                        w.push(self.put(&b));
                        if done {
                            self.downloads.remove(&hash);
                            self.in_flight_ranges.remove(&hash);
                            w.push(Write::Del(kv::download_key(hash)));
                        }
                    }
                }
            }
            BlobResult::Presence {
                present,
                missing: _,
            } => {
                self.presence_in_flight = false;
                for h in present {
                    w.extend(self.server_present(h));
                }
            }
            BlobResult::Failed { task } => match task {
                BlobTask::Begin { hash, .. } => {
                    self.in_flight_begin.remove(&hash);
                }
                BlobTask::PutChunk { hash, index, .. } => {
                    if let Some(s) = self.in_flight_chunks.get_mut(&hash) {
                        s.remove(&index);
                    }
                }
                BlobTask::Complete { hash, .. } => {
                    self.in_flight_complete.remove(&hash);
                }
                BlobTask::GetRange { hash, index, .. } => {
                    if let Some(s) = self.in_flight_ranges.get_mut(&hash) {
                        s.remove(&index);
                    }
                }
                BlobTask::Presence { .. } => self.presence_in_flight = false,
            },
        }
        w
    }

    /// The eviction rule: only confirmed, unpinned blobs may be evicted.
    pub fn can_evict(&self, hash: &Hash) -> bool {
        self.local
            .get(hash)
            .map(|b| b.state == LocalState::Confirmed && !b.pinned)
            .unwrap_or(false)
    }

    /// LRU eviction candidates to get local usage under `cap` bytes.
    pub fn eviction_candidates(&self, cap: u64) -> Vec<Hash> {
        let mut used: u64 = self
            .local
            .values()
            .filter(|b| b.complete_locally(self.chunk))
            .map(|b| b.size)
            .sum();
        let mut c: Vec<&LocalBlob> = self
            .local
            .values()
            .filter(|b| {
                self.can_evict(&b.hash) && b.have.count(chunk_count(b.size, self.chunk)) > 0
            })
            .collect();
        c.sort_by_key(|b| (b.last_access, b.hash));
        let mut out = Vec::new();
        for b in c {
            if used <= cap {
                break;
            }
            used = used.saturating_sub(b.size);
            out.push(b.hash);
        }
        out
    }

    /// Drops local bytes of a blob. Refuses (returns None) if the eviction rule forbids it.
    pub fn evict(&mut self, hash: Hash) -> Option<Vec<Write>> {
        if !self.can_evict(&hash) {
            return None;
        }
        let b = self.local.get_mut(&hash)?;
        b.have = Bitmap::default();
        b.state = LocalState::Confirmed;
        let b = b.clone();
        Some(vec![self.put(&b)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> Hash {
        Hash([n; 32])
    }

    #[test]
    fn upload_flow_and_eviction_rule() {
        let mut m = BlobManager::new(10);
        m.ingest(h(1), 25, 0);
        assert!(!m.can_evict(&h(1)));
        let t = m.next_tasks();
        assert_eq!(
            t,
            vec![BlobTask::Begin {
                hash: h(1),
                size: 25
            }]
        );
        m.on_result(
            BlobResult::Began {
                hash: h(1),
                present: false,
                upload_id: Some("u".into()),
                received: Bitmap::default(),
            },
            0,
        );
        let t = m.next_tasks();
        assert_eq!(t.len(), 2);
        for task in t {
            if let BlobTask::PutChunk { index, .. } = task {
                m.on_result(
                    BlobResult::ChunkDone {
                        hash: h(1),
                        index,
                        ok: true,
                    },
                    0,
                );
            }
        }
        let t = m.next_tasks();
        assert!(matches!(t[0], BlobTask::PutChunk { index: 2, .. }));
        m.on_result(
            BlobResult::ChunkDone {
                hash: h(1),
                index: 2,
                ok: true,
            },
            0,
        );
        assert!(!m.can_evict(&h(1)));
        let t = m.next_tasks();
        assert_eq!(
            t,
            vec![BlobTask::Complete {
                hash: h(1),
                upload_id: "u".into()
            }]
        );
        m.on_result(
            BlobResult::Completed {
                hash: h(1),
                ok: true,
            },
            0,
        );
        assert!(m.can_evict(&h(1)));
        assert!(m.next_tasks().is_empty());
    }

    #[test]
    fn download_priorities() {
        let mut m = BlobManager::new(10);
        m.want(h(1), 5, P3_PREFETCH, 0);
        m.want(h(2), 5, P0_OPEN, 0);
        let t = m.next_tasks();
        assert!(matches!(t[0], BlobTask::GetRange { hash, .. } if hash == h(2)));
    }
}
