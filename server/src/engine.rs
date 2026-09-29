//! The server's single writer (DESIGN §5.2). Owns the write connection, the in-memory metadata
//! state and resolve index, the yrs doc cache and the server HLC. Every mutation of `jess.db`
//! goes through here, one transaction at a time.

use crate::db::{self, hlc_to_bytes};
use crate::error::{Error, Result};
use jess_core::apply::{apply_meta, recover_purged, Ctx, Mode, RenameRec, Tx};
use jess_core::doc as ydoc;
use jess_core::hlc::{Clock, Hlc};
use jess_core::links::{extract, target_key, Link, Syntax};
use jess_core::model::{BlobInfo, Entry, KIND_MARKDOWN, KIND_VAULT, SLOT_BODY};
use jess_core::ops::{AckResult, MetaOp, Op, OpBody, Reject};
use jess_core::resolve::ResolveIndex;
use jess_core::rewrite::{
    format_target, invariant_r_rewrites, stale_link_target, HistoryRow, Rewrite,
};
use jess_core::state::MetaState;
use jess_core::{Hash, Id};
use rand::{rngs::StdRng, Rng, SeedableRng};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

pub const CLOCK_SKEW_MS: u64 = 2_000;
pub const MODIFIED_EVERY_MS: u64 = 60_000;

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub trash_retention_ms: u64,
    pub blob_retention_ms: u64,
    pub max_upload_bytes: u64,
    pub chunk_size: u64,
    pub compact_rows: u64,
    pub compact_age_ms: u64,
    pub upload_expiry_ms: u64,
    pub op_result_retention_ms: u64,
    pub doc_cache: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            trash_retention_ms: 30 * 86_400_000,
            blob_retention_ms: 30 * 86_400_000,
            max_upload_bytes: 2048 << 20,
            chunk_size: jess_core::blobs::CHUNK_SIZE,
            compact_rows: 200,
            compact_age_ms: 3_600_000,
            upload_expiry_ms: 7 * 86_400_000,
            op_result_retention_ms: 30 * 86_400_000,
            doc_cache: 256,
        }
    }
}

struct DocCache {
    map: HashMap<(Id, String), (yrs::Doc, u64)>,
    tick: u64,
    cap: usize,
}

impl DocCache {
    fn drop_doc(&mut self, k: &(Id, String)) {
        self.map.remove(k);
    }
    fn trim(&mut self) {
        while self.map.len() > self.cap {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(k, (_, t))| (*t, (*k).clone()))
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => {
                    self.map.remove(&k);
                }
                None => break,
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct DirtyInfo {
    known_seq: u64,
    replica: u64,
    op_id: u64,
}

#[derive(Default)]
struct Work {
    tx: Tx,
    /// (op_id, ids its prediction changed) for meta ops → ack seq.
    meta_ops: Vec<(u64, Vec<Id>)>,
    /// (rename, origin replica, origin op)
    renames: Vec<(RenameRec, Option<u64>, Option<u64>)>,
    dirty: BTreeMap<(Id, String), DirtyInfo>,
    blob_infos: HashMap<Hash, BlobInfo>,
    acks: Vec<(u64, AckResult)>,
    last_op: u64,
    replica: Option<u64>,
    notes: Vec<String>,
}

pub enum BeginUpload {
    Present,
    Session {
        upload_id: String,
        chunk_size: u64,
        received: Vec<u8>,
    },
    TooLarge,
}

#[derive(Clone, Debug)]
pub struct UploadRow {
    pub upload_id: String,
    pub hash: Hash,
    pub size: u64,
    pub chunk_size: u64,
    pub received: Vec<u8>,
}

#[derive(Debug, Default, Clone)]
pub struct GcReport {
    pub marked: usize,
    pub deleted: Vec<Hash>,
    pub expired_uploads: Vec<String>,
}

/// (source note, link ordinal, link, currently resolved target)
type CandidateLink = (Id, u32, Link, Option<Id>);

pub struct Engine {
    pub conn: Connection,
    pub state: MetaState,
    pub ix: ResolveIndex,
    pub vault_id: Id,
    pub head: u64,
    pub clock: Clock,
    pub cfg: EngineConfig,
    docs: DocCache,
    rng: StdRng,
    /// Fault injection: fail the next commit (simulated crash mid-batch).
    pub fail_next_commit: bool,
}

impl Engine {
    /// Loads everything from the database; initialises a fresh vault if needed.
    pub fn open(conn: Connection, cfg: EngineConfig, seed: u64) -> Result<Engine> {
        let mut rng = StdRng::seed_from_u64(seed);
        let head = db::get_meta_i64(&conn, "head_seq")?.unwrap_or(0) as u64;
        let mut clock = Clock::new(0);
        if let Some(h) = db::get_meta(&conn, "hlc")? {
            if let Ok(h) = minicbor::decode::<Hlc>(&h) {
                clock.observe(h);
            }
        }
        let vault_id = match db::get_meta(&conn, "vault_id")?.and_then(|v| Id::from_slice(&v)) {
            Some(v) => v,
            None => {
                let mut b = [0u8; 10];
                rng.fill(&mut b);
                let v = Id::new_v7(0, b);
                db::set_meta(&conn, "vault_id", &v.0)?;
                v
            }
        };
        let entries = db::load_entries(&conn)?;
        let state = MetaState::from_entries(entries);
        let ix = ResolveIndex::build(&state);
        let cap = cfg.doc_cache;
        let mut e = Engine {
            conn,
            state,
            ix,
            vault_id,
            head,
            clock,
            cfg,
            docs: DocCache {
                map: HashMap::new(),
                tick: 0,
                cap,
            },
            rng,
            fail_next_commit: false,
        };
        if e.state.get(&jess_core::ids::VAULT_SETTINGS_ID).is_none() {
            let op = MetaOp::Create {
                id: jess_core::ids::VAULT_SETTINGS_ID,
                kind: KIND_VAULT.into(),
                parent: None,
                name: ".jess-vault".into(),
                tree_visible: false,
                blob: None,
                blob_info: None,
                created_at: None,
                modified_at: None,
                props: vec![],
            };
            e.server_ops(vec![op], 0)?;
        }
        Ok(e)
    }

    pub fn into_conn(self) -> Connection {
        self.conn
    }

    fn begin(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(())
    }
    fn commit(&mut self) -> Result<()> {
        if self.fail_next_commit {
            self.fail_next_commit = false;
            return Err(Error::Injected);
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }
    fn rollback(&self) {
        let _ = self.conn.execute_batch("ROLLBACK");
    }

    fn next_seq(&mut self) -> u64 {
        self.head += 1;
        self.head
    }

    fn server_ctx(&mut self, now: u64) -> Ctx {
        let hlc = self.clock.now(now);
        Ctx {
            hlc,
            known_seq: self.head,
            replica: 0,
            op_id: hlc.wall ^ ((hlc.logical as u64) << 48),
            mode: Mode::Server,
            now,
        }
    }

    pub fn hlc(&self) -> Hlc {
        self.clock.last
    }

    // ------------------------------------------------------------------ docs

    fn load_doc(&mut self, entry: Id, slot: &str) -> Result<&yrs::Doc> {
        let k = (entry, slot.to_string());
        self.docs.tick += 1;
        let tick = self.docs.tick;
        if !self.docs.map.contains_key(&k) {
            let cid: u64 = self.rng.gen::<u64>() & ((1 << 53) - 1);
            let d = ydoc::new_doc(cid.max(1));
            let mut st = self.conn.prepare_cached(
                "SELECT \"update\" FROM doc_updates WHERE entry_id = ?1 AND slot = ?2 ORDER BY seq",
            )?;
            let rows = st.query_map(params![entry.0.to_vec(), slot], |r| r.get::<_, Vec<u8>>(0))?;
            for u in rows {
                let u = u?;
                if ydoc::apply(&d, &u).is_err() {
                    tracing::error!(%entry, slot, "stored doc update fails to apply");
                }
            }
            drop(st);
            self.docs.map.insert(k.clone(), (d, tick));
            self.docs.trim();
        }
        let v = self.docs.map.get_mut(&k).expect("inserted");
        v.1 = tick;
        Ok(&v.0)
    }

    /// Current text of a markdown doc (loads from the log if needed).
    pub fn doc_text(&mut self, entry: Id, slot: &str) -> Result<String> {
        Ok(ydoc::text(self.load_doc(entry, slot)?))
    }

    pub fn doc_state(&mut self, entry: Id, slot: &str) -> Result<Vec<u8>> {
        Ok(ydoc::encode_state(self.load_doc(entry, slot)?))
    }

    pub fn doc_diff(&mut self, entry: Id, slot: &str, sv: &[u8]) -> Result<Option<Vec<u8>>> {
        let d = self.load_doc(entry, slot)?;
        Ok(ydoc::diff(d, sv).ok())
    }

    /// Raw update rows of a doc, in seq order.
    pub fn doc_rows(&self, entry: Id, slot: &str) -> Result<Vec<Vec<u8>>> {
        let mut st = self.conn.prepare_cached(
            "SELECT \"update\" FROM doc_updates WHERE entry_id = ?1 AND slot = ?2 ORDER BY seq",
        )?;
        let v = st
            .query_map(params![entry.0.to_vec(), slot], |r| r.get::<_, Vec<u8>>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(v)
    }

    fn insert_doc_row(
        &mut self,
        entry: Id,
        slot: &str,
        bytes: &[u8],
        origin: Option<u64>,
        now: u64,
    ) -> Result<u64> {
        let seq = self.next_seq();
        self.conn.prepare_cached("INSERT INTO doc_updates(seq, entry_id, slot, \"update\", origin_replica, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?.execute(params![
            seq as i64,
            entry.0.to_vec(),
            slot,
            bytes,
            origin.map(|r| r as i64),
            now as i64
        ])?;
        Ok(seq)
    }

    // ------------------------------------------------------------------ push

    /// Applies a push from `replica`. Returns per-op results; the batch is committed
    /// (`synchronous=FULL`) before this returns.
    pub fn push(
        &mut self,
        replica: u64,
        device: Option<Id>,
        ops: &[Op],
        now: u64,
    ) -> Result<Vec<(u64, AckResult)>> {
        self.run(now, |e, w| {
            e.push_inner(replica, device, ops, now, w, false)
        })
    }

    /// Server-authored meta ops (trash retention, imports, setup).
    pub fn server_ops(&mut self, ops: Vec<MetaOp>, now: u64) -> Result<Vec<(u64, AckResult)>> {
        let mut built = Vec::new();
        for (i, m) in ops.into_iter().enumerate() {
            let hlc = self.clock.now(now);
            built.push(Op {
                op_id: i as u64 + 1,
                hlc,
                known_seq: self.head,
                group: 0,
                body: OpBody::Meta(m),
            });
        }
        self.run(now, |e, w| e.push_inner(0, None, &built, now, w, true))
    }

    /// Server-authored doc edits (import): creates/extends a doc with `update`.
    pub fn server_doc(&mut self, entry: Id, slot: &str, update: &[u8], now: u64) -> Result<u64> {
        let op = Op {
            op_id: 1,
            hlc: self.clock.now(now),
            known_seq: self.head,
            group: 0,
            body: OpBody::Doc {
                entry,
                slot: slot.into(),
                update: update.to_vec(),
            },
        };
        let r = self.run(now, |e, w| {
            e.push_inner(0, None, std::slice::from_ref(&op), now, w, true)
        })?;
        match r.first() {
            Some((_, AckResult::Applied(s))) => Ok(*s),
            other => Err(Error::Other(format!("server doc edit failed: {other:?}"))),
        }
    }

    fn run(
        &mut self,
        now: u64,
        f: impl FnOnce(&mut Engine, &mut Work) -> Result<()>,
    ) -> Result<Vec<(u64, AckResult)>> {
        self.begin()?;
        let head0 = self.head;
        let mut w = Work::default();
        let r = f(self, &mut w)
            .and_then(|_| self.finish(&mut w, head0, now))
            .and_then(|_| self.commit());
        match r {
            Ok(()) => {
                for n in &w.notes {
                    tracing::info!("{n}");
                }
                Ok(std::mem::take(&mut w.acks))
            }
            Err(e) => {
                self.rollback();
                self.head = head0;
                let touched: Vec<Id> = w.tx.touched().copied().collect();
                std::mem::take(&mut w.tx).rollback(&mut self.state);
                self.ix.sync(&self.state, &touched);
                // Cached docs may contain uncommitted updates.
                self.docs.map.clear();
                Err(e)
            }
        }
    }

    fn push_inner(
        &mut self,
        replica: u64,
        device: Option<Id>,
        ops: &[Op],
        now: u64,
        w: &mut Work,
        internal: bool,
    ) -> Result<()> {
        let (mut last_op, pruned_upto): (u64, u64) = if internal {
            (0, 0)
        } else {
            self.conn
                .query_row(
                    "SELECT last_op_id, pruned_upto FROM replicas WHERE replica_id = ?1",
                    [replica as i64],
                    |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
                )
                .optional()?
                .unwrap_or((0, 0))
        };
        let mut i = 0;
        while i < ops.len() {
            // A group: consecutive meta ops with the same non-zero group id.
            let mut j = i + 1;
            if ops[i].group != 0 && matches!(ops[i].body, OpBody::Meta(_)) {
                while j < ops.len()
                    && ops[j].group == ops[i].group
                    && matches!(ops[j].body, OpBody::Meta(_))
                {
                    j += 1;
                }
            }
            let group = &ops[i..j];
            i = j;
            // Dedupe by exact (replica, op_id): pushes may arrive reordered (HTTP fallback).
            let prev = if internal {
                None
            } else {
                self.op_result(replica, group[0].op_id)?
            };
            if !internal && (prev.is_some() || group[0].op_id <= pruned_upto) {
                for op in group {
                    let ack = match self.op_result(replica, op.op_id)? {
                        // A replayed op gets its original outcome (a rejection stays a rejection).
                        Some(AckResult::Rejected(r)) => AckResult::Rejected(r),
                        Some(AckResult::Applied(s)) | Some(AckResult::Duplicate(s)) => {
                            AckResult::Duplicate(s)
                        }
                        None => AckResult::Duplicate(0),
                    };
                    w.acks.push((op.op_id, ack));
                }
                continue;
            }
            self.conn.execute_batch("SAVEPOINT grp")?;
            let mut gtx = Tx::new();
            let mut results: Vec<(u64, std::result::Result<Option<u64>, Reject>)> = Vec::new();
            let mut group_renames = Vec::new();
            let mut group_meta = Vec::new();
            let mut failed = None;
            for op in group {
                let mut hlc = op.hlc;
                hlc.wall = hlc.wall.min(now + CLOCK_SKEW_MS);
                self.clock.observe(hlc);
                let ctx = Ctx {
                    hlc,
                    known_seq: op.known_seq,
                    replica,
                    op_id: op.op_id,
                    mode: Mode::Server,
                    now,
                };
                let r = match &op.body {
                    OpBody::Meta(m) => {
                        let before = gtx.renames.len();
                        let mut optx = Tx::new();
                        let r = apply_meta(&mut self.state, &mut optx, m, &ctx);
                        if r.is_ok() && !optx.purged.is_empty() {
                            // Purge doc rows now, so a later op in this transaction can recover them.
                            let ids = optx.purged.clone();
                            if let Err(e) = self.purge_docs(&ids, w) {
                                tracing::error!("purge failed: {e}");
                                return Err(e);
                            }
                        }
                        if r.is_ok() {
                            group_meta.push((op.op_id, optx.changed(&self.state)));
                            let _ = before;
                            for rr in &optx.renames {
                                group_renames.push((
                                    rr.clone(),
                                    Some(replica).filter(|_| !internal),
                                    Some(op.op_id).filter(|_| !internal),
                                ));
                            }
                            if let MetaOp::Create {
                                blob: Some(h),
                                blob_info: Some(bi),
                                ..
                            }
                            | MetaOp::SetBlob {
                                blob: h,
                                blob_info: Some(bi),
                                ..
                            } = m
                            {
                                w.blob_infos.insert(*h, bi.clone());
                            }
                            gtx.absorb(optx);
                        }
                        r.map(|_| None)
                    }
                    OpBody::Doc {
                        entry,
                        slot,
                        update,
                    } => self.apply_doc(
                        &mut gtx, w, *entry, slot, update, &ctx, replica, internal, now,
                    ),
                };
                if let Err(reason) = r {
                    if group.len() > 1 {
                        failed = Some((op.op_id, reason));
                        break;
                    }
                }
                results.push((op.op_id, r));
            }
            if let Some((bad, reason)) = failed {
                // Atomic group: undo everything (memory and SQL), reject all.
                gtx.rollback(&mut self.state);
                self.conn.execute_batch("ROLLBACK TO grp; RELEASE grp")?;
                self.docs.map.clear();
                for op in group {
                    let r = if op.op_id == bad {
                        reason
                    } else {
                        Reject::GroupFailed
                    };
                    w.acks.push((op.op_id, AckResult::Rejected(r)));
                }
            } else {
                self.conn.execute_batch("RELEASE grp")?;
                for (op_id, r) in results {
                    match r {
                        Ok(Some(0)) => w.acks.push((op_id, AckResult::Duplicate(0))),
                        Ok(Some(seq)) => w.acks.push((op_id, AckResult::Applied(seq))),
                        Ok(None) => w.acks.push((op_id, AckResult::Applied(0))), // seq filled in finish()
                        Err(reason) => w.acks.push((op_id, AckResult::Rejected(reason))),
                    }
                }
                w.meta_ops.extend(group_meta);
                w.renames.extend(group_renames);
                w.notes.extend(std::mem::take(&mut gtx.notes));
                w.tx.absorb(gtx);
            }
            if !internal {
                last_op = last_op.max(group.iter().map(|o| o.op_id).max().unwrap_or(0));
            }
        }
        w.last_op = last_op;
        w.replica = if internal { None } else { Some(replica) };
        if !internal {
            self.conn.execute(
                "INSERT INTO replicas(replica_id, device_id, last_op_id, last_seen) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(replica_id) DO UPDATE SET last_op_id = max(last_op_id, excluded.last_op_id), last_seen = excluded.last_seen, device_id = coalesce(excluded.device_id, device_id)",
                params![replica as i64, device.map(|d| d.0.to_vec()), last_op as i64, now as i64],
            )?;
        }
        let _ = replica;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_doc(
        &mut self,
        gtx: &mut Tx,
        w: &mut Work,
        entry: Id,
        slot: &str,
        update: &[u8],
        ctx: &Ctx,
        replica: u64,
        internal: bool,
        now: u64,
    ) -> std::result::Result<Option<u64>, Reject> {
        let Some(e) = self.state.get(&entry) else {
            return Err(Reject::UnknownEntry);
        };
        if e.is_folder() || e.kind == KIND_VAULT {
            return Err(Reject::Forbidden);
        }
        if slot.is_empty() || slot.len() > 64 {
            return Err(Reject::Forbidden);
        }
        let origin = if internal { None } else { Some(replica) };
        if e.purged {
            // Recovered-after-purge (§6.4).
            let purged: Option<(Vec<u8>, Vec<u8>)> = self
                .conn
                .query_row(
                    "SELECT state_vector, state FROM doc_purged WHERE entry_id = ?1 AND slot = ?2",
                    params![entry.0.to_vec(), slot],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|_| Reject::BadUpdate)?;
            let (_sv, state) = purged.unwrap_or_default();
            if !ydoc::has_new_content(update, &state).map_err(|_| Reject::BadUpdate)? {
                // Fully covered by the purged state: nothing to keep (acked as Duplicate(0)).
                return Ok(Some(0));
            }
            let sctx = self.server_ctx(now);
            recover_purged(&mut self.state, gtx, entry, &sctx);
            let full = if state.is_empty() {
                update.to_vec()
            } else {
                ydoc::merge(&[state, update.to_vec()]).map_err(|_| Reject::BadUpdate)?
            };
            let _ = self.conn.execute(
                "DELETE FROM doc_purged WHERE entry_id = ?1 AND slot = ?2",
                params![entry.0.to_vec(), slot],
            );
            let seq = self
                .insert_doc_row(entry, slot, &full, None, now)
                .map_err(|_| Reject::BadUpdate)?;
            self.docs.drop_doc(&(entry, slot.to_string()));
            w.dirty.insert(
                (entry, slot.to_string()),
                DirtyInfo {
                    known_seq: ctx.known_seq,
                    replica,
                    op_id: ctx.op_id,
                },
            );
            return Ok(Some(seq));
        }
        let parsed = ydoc::decode(update).map_err(|_| Reject::BadUpdate)?;
        drop(parsed);
        let k = (entry, slot.to_string());
        let ok = match self.load_doc(entry, slot) {
            Ok(d) => ydoc::apply(d, update).is_ok(),
            Err(_) => false,
        };
        if !ok {
            self.docs.drop_doc(&k);
            return Err(Reject::BadUpdate);
        }
        let seq = self
            .insert_doc_row(entry, slot, update, origin, now)
            .map_err(|_| Reject::BadUpdate)?;
        let di = w.dirty.entry(k).or_insert(DirtyInfo {
            known_seq: ctx.known_seq,
            replica,
            op_id: ctx.op_id,
        });
        di.known_seq = di.known_seq.min(ctx.known_seq);
        // modified_at, at most once a minute per doc (§3.1).
        let stale = self
            .state
            .get(&entry)
            .and_then(|e| e.modified_at)
            .map(|m| now >= m + MODIFIED_EVERY_MS)
            .unwrap_or(true);
        if stale {
            let sctx = self.server_ctx(now);
            let _ = apply_meta(
                &mut self.state,
                gtx,
                &MetaOp::SetTimes {
                    id: entry,
                    created: None,
                    modified: Some(now),
                },
                &sctx,
            );
        }
        Ok(Some(seq))
    }

    // ------------------------------------------------------------------ finish: link pass, rows

    fn finish(&mut self, w: &mut Work, head0: u64, now: u64) -> Result<()> {
        let touched: Vec<Id> = w.tx.touched().copied().collect();
        self.ix.sync(&self.state, &touched);
        self.link_pass(w, now)?;
        // Blob references.
        let changed = w.tx.changed(&self.state);
        for id in &changed {
            if let Some(h) = self.state.get(id).and_then(|e| e.blob) {
                self.ensure_blob_row(h, w.blob_infos.get(&h).cloned())?;
            }
        }
        // Entry rows with fresh seqs.
        let mut seqs: HashMap<Id, u64> = HashMap::new();
        for id in &changed {
            if self.state.get(id).is_some() {
                self.conn
                    .execute("DELETE FROM entries WHERE id = ?1", [id.0.to_vec()])?;
            }
        }
        for id in &changed {
            let Some(e) = self.state.get(id).cloned() else {
                continue;
            };
            let seq = self.next_seq();
            let mut e = e;
            e.seq = seq;
            let purged_at = if e.purged { Some(now) } else { None };
            db::put_entry(&self.conn, &e, purged_at)?;
            self.state.put(e);
            seqs.insert(*id, seq);
        }
        for (r, origin, op) in &w.renames {
            let seq = seqs.get(&r.entry).copied().unwrap_or(self.head);
            self.conn.execute(
                "INSERT INTO rename_history(seq, entry_id, old_path, new_path, is_folder, origin_replica, origin_op, induced) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![seq as i64, r.entry.0.to_vec(), r.old_path, r.new_path, r.is_folder as i64, origin.map(|v| v as i64), op.map(|v| v as i64), r.induced as i64],
            )?;
        }
        // Ack seqs for meta ops: the max seq of rows they changed, else the current head.
        let meta_seq: HashMap<u64, u64> = w
            .meta_ops
            .iter()
            .map(|(op, ids)| {
                (
                    *op,
                    ids.iter()
                        .filter_map(|i| seqs.get(i))
                        .copied()
                        .max()
                        .unwrap_or(self.head),
                )
            })
            .collect();
        for (op, r) in w.acks.iter_mut() {
            if let AckResult::Applied(s) = r {
                if *s == 0 {
                    *s = meta_seq.get(op).copied().unwrap_or(self.head);
                }
            }
        }
        if self.head > head0 {
            self.conn.execute(
                "INSERT INTO commits(last_seq, first_seq) VALUES (?1, ?2)",
                params![self.head as i64, (head0 + 1) as i64],
            )?;
        }
        db::set_meta_i64(&self.conn, "head_seq", self.head as i64)?;
        db::set_meta(&self.conn, "hlc", &hlc_to_bytes(self.clock.last))?;
        // Idempotency records, in the same transaction (skip server-internal ops).
        if let Some(replica) = w.replica {
            let mut st = self.conn.prepare_cached("INSERT OR REPLACE INTO op_results(replica_id, op_id, seq, result, at) VALUES (?1, ?2, ?3, ?4, ?5)")?;
            for (op, r) in &w.acks {
                let seq = match r {
                    AckResult::Applied(s) => *s as i64,
                    AckResult::Duplicate(_) => continue,
                    AckResult::Rejected(_) => 0,
                };
                st.execute(params![
                    replica as i64,
                    *op as i64,
                    seq,
                    minicbor::to_vec(r).expect("encode"),
                    now as i64
                ])?;
            }
        }
        Ok(())
    }

    fn op_result(&self, replica: u64, op_id: u64) -> Result<Option<AckResult>> {
        let r: Option<Vec<u8>> = self
            .conn
            .prepare_cached("SELECT result FROM op_results WHERE replica_id = ?1 AND op_id = ?2")?
            .query_row(params![replica as i64, op_id as i64], |r| r.get(0))
            .optional()?;
        Ok(r.and_then(|b| minicbor::decode::<AckResult>(&b).ok()))
    }

    fn ensure_blob_row(&mut self, h: Hash, info: Option<BlobInfo>) -> Result<()> {
        let exists: Option<i64> = self
            .conn
            .query_row(
                "SELECT present FROM blobs WHERE hash = ?1",
                [h.0.to_vec()],
                |r| r.get(0),
            )
            .optional()?;
        match exists {
            Some(_) => {
                self.conn.execute(
                    "UPDATE blobs SET unreferenced_since = NULL WHERE hash = ?1",
                    [h.0.to_vec()],
                )?;
            }
            None => {
                let seq = self.next_seq();
                let i = info.unwrap_or_default();
                self.conn.execute(
                    "INSERT INTO blobs(hash, size, mime, width, height, orientation, present, seq) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
                    params![h.0.to_vec(), i.size as i64, i.mime, i.width.map(|v| v as i64), i.height.map(|v| v as i64), i.orientation.map(|v| v as i64), seq as i64],
                )?;
            }
        }
        Ok(())
    }

    fn purge_docs(&mut self, purged: &[Id], w: &mut Work) -> Result<()> {
        for &id in purged {
            let slots: Vec<String> = {
                let mut st = self
                    .conn
                    .prepare("SELECT DISTINCT slot FROM doc_updates WHERE entry_id = ?1")?;
                let v = st
                    .query_map([id.0.to_vec()], |r| r.get(0))?
                    .collect::<std::result::Result<Vec<String>, _>>()?;
                v
            };
            for slot in slots {
                // Merge the raw rows (not encode_state): keeps updates still pending on missing deps.
                let rows = self.doc_rows(id, &slot)?;
                let st = ydoc::merge(&rows).map_err(|_| Error::Other("merge failed".into()))?;
                let sv = yrs::encode_state_vector_from_update_v1(&st)
                    .map_err(|_| Error::Other("sv failed".into()))?;
                self.conn.execute("INSERT OR REPLACE INTO doc_purged(entry_id, slot, state_vector, state) VALUES (?1, ?2, ?3, ?4)", params![id.0.to_vec(), slot, sv, st])?;
                self.conn.execute(
                    "DELETE FROM doc_updates WHERE entry_id = ?1 AND slot = ?2",
                    params![id.0.to_vec(), slot],
                )?;
                self.docs.drop_doc(&(id, slot.clone()));
                w.dirty.remove(&(id, slot));
            }
            self.conn
                .execute("DELETE FROM links WHERE src = ?1", [id.0.to_vec()])?;
        }
        Ok(())
    }

    fn load_links(&self, src: Id, slot: &str) -> Result<Vec<(u32, Link, Option<Id>)>> {
        let mut st = self.conn.prepare_cached(
            "SELECT ord, start16, end16, tstart16, tend16, syntax, embed, target, raw_target, angle, subpath, display, resolved FROM links WHERE src = ?1 AND slot = ?2 ORDER BY ord",
        )?;
        let v = st
            .query_map(params![src.0.to_vec(), slot], |r| {
                Ok((
                    r.get::<_, i64>(0)? as u32,
                    Link {
                        range: (r.get::<_, i64>(1)? as u32, r.get::<_, i64>(2)? as u32),
                        target_range: (r.get::<_, i64>(3)? as u32, r.get::<_, i64>(4)? as u32),
                        syntax: if r.get::<_, i64>(5)? == 0 {
                            Syntax::Wiki
                        } else {
                            Syntax::Markdown
                        },
                        embed: r.get::<_, i64>(6)? != 0,
                        target: r.get(7)?,
                        raw_target: r.get(8)?,
                        angle: r.get::<_, i64>(9)? != 0,
                        subpath: r.get(10)?,
                        display: r.get(11)?,
                    },
                    r.get::<_, Option<Vec<u8>>>(12)?
                        .and_then(|b| Id::from_slice(&b)),
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(v)
    }

    fn write_links(&self, src: Id, slot: &str, links: &[Link]) -> Result<()> {
        self.conn.execute(
            "DELETE FROM links WHERE src = ?1 AND slot = ?2",
            params![src.0.to_vec(), slot],
        )?;
        let folder = self.state.folder_of(src);
        let mut st = self.conn.prepare_cached(
            "INSERT INTO links(src, slot, ord, start16, end16, tstart16, tend16, syntax, embed, target, raw_target, angle, target_key, subpath, display, resolved)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        )?;
        for (i, l) in links.iter().enumerate() {
            let resolved = self
                .ix
                .resolve(&l.target, l.syntax, &folder)
                .map(|r| r.id.0.to_vec());
            st.execute(params![
                src.0.to_vec(),
                slot,
                i as i64,
                l.range.0 as i64,
                l.range.1 as i64,
                l.target_range.0 as i64,
                l.target_range.1 as i64,
                (l.syntax == Syntax::Markdown) as i64,
                l.embed as i64,
                l.target,
                l.raw_target,
                l.angle as i64,
                target_key(&l.target),
                l.subpath,
                l.display,
                resolved
            ])?;
        }
        Ok(())
    }

    fn is_markdown(&self, id: &Id) -> bool {
        self.state
            .get(id)
            .map(|e| e.kind == KIND_MARKDOWN && !e.purged)
            .unwrap_or(false)
    }

    fn link_pass(&mut self, w: &mut Work, now: u64) -> Result<()> {
        // 1. Re-extract dirty docs; find newly introduced links.
        let mut introduced: Vec<(Id, u32, Link, DirtyInfo)> = Vec::new();
        let dirty: Vec<((Id, String), DirtyInfo)> =
            w.dirty.iter().map(|(k, v)| (k.clone(), *v)).collect();
        let mut reextracted: HashSet<Id> = HashSet::new();
        for ((src, slot), info) in &dirty {
            if slot != SLOT_BODY || !self.is_markdown(src) {
                continue;
            }
            let text = self.doc_text(*src, slot)?;
            let ext = extract(&text);
            let old = self.load_links(*src, slot)?;
            let mut counts: HashMap<(Syntax, bool, String, Option<String>), usize> = HashMap::new();
            for (_, l, _) in &old {
                *counts
                    .entry((l.syntax, l.embed, l.target.clone(), l.subpath.clone()))
                    .or_default() += 1;
            }
            for (i, l) in ext.links.iter().enumerate() {
                let k = (l.syntax, l.embed, l.target.clone(), l.subpath.clone());
                match counts.get_mut(&k) {
                    Some(n) if *n > 0 => *n -= 1,
                    _ => introduced.push((*src, i as u32, l.clone(), *info)),
                }
            }
            self.write_links(*src, slot, &ext.links)?;
            reextracted.insert(*src);
        }
        let changed: Vec<Id> = w.tx.changed(&self.state);
        if changed.is_empty() && introduced.is_empty() {
            return Ok(());
        }
        let mut rewrites: Vec<Rewrite> = Vec::new();
        let intro_set: HashSet<(Id, u32)> =
            introduced.iter().map(|(s, o, _, _)| (*s, *o)).collect();
        // 2. Candidate links affected by metadata changes.
        let mut before = self.state.clone();
        for id in w.tx.touched() {
            match w.tx.original(id) {
                Some(Some(e)) => {
                    before.put(e.clone());
                }
                Some(None) => {
                    before.remove(id);
                }
                None => {}
            }
        }
        let mut s_set: BTreeSet<Id> = BTreeSet::new();
        for id in &changed {
            s_set.insert(*id);
            for st in [&self.state, &before] {
                if st.get(id).map(|e| e.is_folder()).unwrap_or(false) {
                    s_set.extend(st.descendants(*id));
                }
            }
        }
        let mut keys: BTreeSet<String> = BTreeSet::new();
        for id in &s_set {
            for st in [&self.state, &before] {
                if let Some(e) = st.get(id) {
                    keys.insert(target_key(&e.name));
                }
            }
        }
        let cands = self.candidate_links(&keys, &s_set)?;
        let has_renames = !w.renames.is_empty();
        if has_renames {
            let mut before_ix = self.ix.clone();
            let ids: Vec<Id> = s_set.iter().copied().collect();
            before_ix.sync(&before, &ids);
            let list: Vec<(Id, &Link)> = cands
                .iter()
                .filter(|(s, o, _, _)| !intro_set.contains(&(*s, *o)))
                .map(|(s, _, l, _)| (*s, l))
                .collect();
            rewrites.extend(invariant_r_rewrites(
                &before,
                &before_ix,
                &self.state,
                &self.ix,
                list,
            ));
        }
        // 3. Stale links introduced by replicas that hadn't seen renames.
        if !introduced.is_empty() {
            let min_known = introduced.iter().map(|x| x.3.known_seq).min().unwrap_or(0);
            let mut history: Vec<HistoryRow> = {
                let mut st = self.conn.prepare_cached("SELECT seq, entry_id, old_path, new_path, is_folder, origin_replica, origin_op, induced FROM rename_history WHERE seq > ?1 ORDER BY seq")?;
                let v = st
                    .query_map([min_known as i64], |r| {
                        let e: Vec<u8> = r.get(1)?;
                        Ok(HistoryRow {
                            seq: r.get::<_, i64>(0)? as u64,
                            rec: RenameRec {
                                entry: Id::from_slice(&e).unwrap_or_default(),
                                old_path: r.get(2)?,
                                new_path: r.get(3)?,
                                is_folder: r.get::<_, i64>(4)? != 0,
                                induced: r.get::<_, i64>(7)? != 0,
                            },
                            origin_replica: r.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                            origin_op: r.get::<_, Option<i64>>(6)?.map(|v| v as u64),
                        })
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                v
            };
            for (r, origin, op) in &w.renames {
                history.push(HistoryRow {
                    seq: u64::MAX,
                    rec: r.clone(),
                    origin_replica: *origin,
                    origin_op: *op,
                });
            }
            if !history.is_empty() {
                for (src, _, link, info) in &introduced {
                    let folder = self.state.folder_of(*src);
                    if let Some(t) = stale_link_target(
                        &self.ix,
                        link,
                        &folder,
                        &history,
                        info.known_seq,
                        info.replica,
                        info.op_id,
                    ) {
                        if let Some(new) = format_target(&self.ix, t.id, link, t.via, &folder) {
                            let old = jess_core::rewrite::written_target(link).to_string();
                            if new != old {
                                rewrites.push(Rewrite {
                                    src: *src,
                                    start16: link.target_range.0,
                                    end16: link.target_range.1,
                                    old,
                                    new,
                                });
                            }
                        }
                    }
                }
            }
        }
        // 4. Apply rewrites as server-authored yrs edits.
        let mut by_src: BTreeMap<Id, Vec<Rewrite>> = BTreeMap::new();
        for r in rewrites {
            let v = by_src.entry(r.src).or_default();
            if !v.iter().any(|x| x.start16 < r.end16 && r.start16 < x.end16) {
                v.push(r);
            }
        }
        for (src, rws) in by_src {
            if !self.is_markdown(&src) {
                continue;
            }
            let d = self.load_doc(src, SLOT_BODY)?;
            let update = ydoc::apply_rewrites(d, &rws);
            self.insert_doc_row(src, SLOT_BODY, &update, None, now)?;
            let text = self.doc_text(src, SLOT_BODY)?;
            let ext = extract(&text);
            self.write_links(src, SLOT_BODY, &ext.links)?;
            reextracted.insert(src);
        }
        // 5. Refresh `resolved` for other affected links.
        for (src, ord, l, resolved) in &cands {
            if reextracted.contains(src) {
                continue;
            }
            let folder = self.state.folder_of(*src);
            let now_r = self.ix.resolve(&l.target, l.syntax, &folder).map(|r| r.id);
            if now_r != *resolved {
                self.conn.execute(
                    "UPDATE links SET resolved = ?1 WHERE src = ?2 AND slot = ?3 AND ord = ?4",
                    params![
                        now_r.map(|i| i.0.to_vec()),
                        src.0.to_vec(),
                        SLOT_BODY,
                        *ord as i64
                    ],
                )?;
            }
        }
        Ok(())
    }

    fn candidate_links(
        &self,
        keys: &BTreeSet<String>,
        ids: &BTreeSet<Id>,
    ) -> Result<Vec<CandidateLink>> {
        let mut srcs: BTreeSet<Id> = BTreeSet::new();
        {
            let mut st = self
                .conn
                .prepare_cached("SELECT DISTINCT src FROM links WHERE target_key = ?1")?;
            for k in keys {
                for s in st.query_map([k], |r| r.get::<_, Vec<u8>>(0))? {
                    if let Some(i) = Id::from_slice(&s?) {
                        srcs.insert(i);
                    }
                }
            }
            let mut st2 = self
                .conn
                .prepare_cached("SELECT DISTINCT src FROM links WHERE resolved = ?1")?;
            for id in ids {
                for s in st2.query_map([id.0.to_vec()], |r| r.get::<_, Vec<u8>>(0))? {
                    if let Some(i) = Id::from_slice(&s?) {
                        srcs.insert(i);
                    }
                }
            }
        }
        for id in ids {
            if self.is_markdown(id) {
                srcs.insert(*id);
            }
        }
        let mut out = Vec::new();
        for src in srcs {
            let own = ids.contains(&src);
            for (ord, l, resolved) in self.load_links(src, SLOT_BODY)? {
                let relevant = own
                    || keys.contains(&target_key(&l.target))
                    || resolved.map(|r| ids.contains(&r)).unwrap_or(false);
                if relevant {
                    out.push((src, ord, l, resolved));
                }
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------------ maintenance

    /// Merges per-doc update rows ≤ X into one row at X (§5.6). Returns docs compacted.
    pub fn compact(&mut self, now: u64, max_docs: usize) -> Result<usize> {
        let docs: Vec<(Vec<u8>, String)> = {
            let mut st = self.conn.prepare(
                "SELECT entry_id, slot FROM doc_updates GROUP BY entry_id, slot HAVING count(*) > 1 AND (count(*) > ?1 OR min(created_at) < ?2) LIMIT ?3",
            )?;
            let v = st
                .query_map(
                    params![
                        self.cfg.compact_rows as i64,
                        now.saturating_sub(self.cfg.compact_age_ms) as i64,
                        max_docs as i64
                    ],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            v
        };
        let mut n = 0;
        for (e, slot) in docs {
            self.begin()?;
            let r = (|| -> Result<()> {
                let rows: Vec<(i64, Vec<u8>)> = {
                    let mut st = self.conn.prepare("SELECT seq, \"update\" FROM doc_updates WHERE entry_id = ?1 AND slot = ?2 ORDER BY seq")?;
                    let v = st
                        .query_map(params![e, slot], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    v
                };
                let Some(&(x, _)) = rows.last() else {
                    return Ok(());
                };
                let ups: Vec<Vec<u8>> = rows.into_iter().map(|(_, u)| u).collect();
                let merged = ydoc::merge(&ups).map_err(|_| Error::Other("merge failed".into()))?;
                self.conn.execute(
                    "DELETE FROM doc_updates WHERE entry_id = ?1 AND slot = ?2 AND seq < ?3",
                    params![e, slot, x],
                )?;
                self.conn.execute("UPDATE doc_updates SET \"update\" = ?1, origin_replica = NULL, merged = 1, created_at = ?2 WHERE seq = ?3", params![merged, now as i64, x])?;
                Ok(())
            })();
            match r.and_then(|_| self.commit()) {
                Ok(()) => n += 1,
                Err(err) => {
                    self.rollback();
                    return Err(err);
                }
            }
        }
        Ok(n)
    }

    /// Purges trash older than the retention period (§6.4). Returns entries purged.
    pub fn purge_expired_trash(&mut self, now: u64) -> Result<usize> {
        let cutoff = now.saturating_sub(self.cfg.trash_retention_ms);
        let roots: Vec<Id> = self
            .state
            .iter()
            .filter(|e| !e.purged)
            .filter(|e| e.trashed.map(|t| t.at < cutoff).unwrap_or(false))
            .filter(|e| {
                e.parent
                    .and_then(|p| self.state.get(&p))
                    .map(|p| p.trashed.is_none())
                    .unwrap_or(true)
            })
            .map(|e| e.id)
            .collect();
        if roots.is_empty() {
            return Ok(0);
        }
        let mut roots = roots;
        roots.sort();
        let n = roots.len();
        self.server_ops(
            roots.into_iter().map(|id| MetaOp::Purge { id }).collect(),
            now,
        )?;
        Ok(n)
    }

    /// Drops idempotency records older than the retention period, remembering per replica the
    /// highest pruned op id (ops at or below it are answered `Duplicate`).
    pub fn prune_op_results(&mut self, now: u64) -> Result<usize> {
        let cutoff = now.saturating_sub(self.cfg.op_result_retention_ms) as i64;
        self.conn.execute(
            "UPDATE replicas SET pruned_upto = max(pruned_upto, ifnull((SELECT max(op_id) FROM op_results o WHERE o.replica_id = replicas.replica_id AND o.at < ?1), 0))",
            [cutoff],
        )?;
        Ok(self
            .conn
            .execute("DELETE FROM op_results WHERE at < ?1", [cutoff])?)
    }

    // ------------------------------------------------------------------ blobs

    pub fn blob_present(&self, h: &Hash) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT present FROM blobs WHERE hash = ?1",
                [h.0.to_vec()],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|p| p != 0)
            .unwrap_or(false))
    }

    pub fn upload_begin(
        &mut self,
        h: Hash,
        size: u64,
        device: Option<Id>,
        now: u64,
    ) -> Result<BeginUpload> {
        if self.blob_present(&h)? {
            return Ok(BeginUpload::Present);
        }
        if size > self.cfg.max_upload_bytes {
            return Ok(BeginUpload::TooLarge);
        }
        let existing: Option<(String, i64, Vec<u8>, i64)> = self
            .conn
            .query_row("SELECT upload_id, chunk_size, received, size FROM uploads WHERE hash = ?1 ORDER BY created_at DESC LIMIT 1", [h.0.to_vec()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<Vec<u8>>>(2)?.unwrap_or_default(), r.get(3)?))
            })
            .optional()?;
        if let Some((id, cs, rec, sz)) = existing {
            if sz as u64 == size {
                self.conn.execute(
                    "UPDATE uploads SET touched_at = ?1 WHERE upload_id = ?2",
                    params![now as i64, id],
                )?;
                return Ok(BeginUpload::Session {
                    upload_id: id,
                    chunk_size: cs as u64,
                    received: rec,
                });
            }
        }
        let mut b = [0u8; 16];
        self.rng.fill(&mut b);
        let id = hex::encode(b);
        self.conn.execute(
            "INSERT INTO uploads(upload_id, hash, size, chunk_size, received, created_at, touched_at, device_id) VALUES (?1, ?2, ?3, ?4, x'', ?5, ?5, ?6)",
            params![id, h.0.to_vec(), size as i64, self.cfg.chunk_size as i64, now as i64, device.map(|d| d.0.to_vec())],
        )?;
        Ok(BeginUpload::Session {
            upload_id: id,
            chunk_size: self.cfg.chunk_size,
            received: vec![],
        })
    }

    pub fn upload_get(&self, upload_id: &str) -> Result<Option<UploadRow>> {
        Ok(self
            .conn
            .query_row("SELECT upload_id, hash, size, chunk_size, received FROM uploads WHERE upload_id = ?1", [upload_id], |r| {
                let h: Vec<u8> = r.get(1)?;
                Ok(UploadRow {
                    upload_id: r.get(0)?,
                    hash: Hash::from_slice(&h).unwrap_or_default(),
                    size: r.get::<_, i64>(2)? as u64,
                    chunk_size: r.get::<_, i64>(3)? as u64,
                    received: r.get::<_, Option<Vec<u8>>>(4)?.unwrap_or_default(),
                })
            })
            .optional()?)
    }

    /// Marks chunk `index` received (after the bytes were written and fsynced).
    pub fn upload_chunk_done(&mut self, upload_id: &str, index: u32, now: u64) -> Result<bool> {
        let Some(u) = self.upload_get(upload_id)? else {
            return Ok(false);
        };
        let mut bm = jess_core::blobs::Bitmap(u.received);
        bm.set(index);
        self.conn.execute(
            "UPDATE uploads SET received = ?1, touched_at = ?2 WHERE upload_id = ?3",
            params![bm.0, now as i64, upload_id],
        )?;
        Ok(true)
    }

    /// The blob file was verified and moved into place: mark present (new seq) and end the session.
    pub fn blob_stored(
        &mut self,
        h: Hash,
        size: u64,
        upload_id: Option<&str>,
        now: u64,
    ) -> Result<()> {
        self.begin()?;
        let head0 = self.head;
        let r = (|| -> Result<()> {
            let seq = self.next_seq();
            self.conn.execute(
                "INSERT INTO blobs(hash, size, present, stored_at, seq) VALUES (?1, ?2, 1, ?3, ?4)
                 ON CONFLICT(hash) DO UPDATE SET size = excluded.size, present = 1, stored_at = excluded.stored_at, seq = excluded.seq",
                params![h.0.to_vec(), size as i64, now as i64, seq as i64],
            )?;
            if let Some(u) = upload_id {
                self.conn
                    .execute("DELETE FROM uploads WHERE upload_id = ?1", [u])?;
            }
            self.conn.execute(
                "INSERT INTO commits(last_seq, first_seq) VALUES (?1, ?2)",
                params![self.head as i64, (head0 + 1) as i64],
            )?;
            db::set_meta_i64(&self.conn, "head_seq", self.head as i64)?;
            Ok(())
        })();
        match r.and_then(|_| self.commit()) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.rollback();
                self.head = head0;
                Err(e)
            }
        }
    }

    pub fn upload_discard(&mut self, upload_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM uploads WHERE upload_id = ?1", [upload_id])?;
        Ok(())
    }

    pub fn presence(&self, hashes: &[Hash]) -> Result<Vec<Hash>> {
        let mut out = Vec::new();
        for h in hashes {
            if self.blob_present(h)? {
                out.push(*h);
            }
        }
        Ok(out)
    }

    /// Blobs referenced by live entries but not present on the server.
    pub fn missing_blobs(&self) -> Result<Vec<Hash>> {
        let mut st = self.conn.prepare("SELECT DISTINCT e.blob FROM entries e LEFT JOIN blobs b ON b.hash = e.blob WHERE e.blob IS NOT NULL AND e.purged = 0 AND (b.present IS NULL OR b.present = 0)")?;
        let v = st
            .query_map([], |r| r.get::<_, Vec<u8>>(0))?
            .filter_map(|r| r.ok())
            .filter_map(|b| Hash::from_slice(&b))
            .collect();
        Ok(v)
    }

    /// Blob GC (§7.8). Marks unreferenced blobs, and returns blobs whose files may now be deleted
    /// (their rows are already `present = 0` and committed). With `dry_run` nothing is changed.
    pub fn gc(&mut self, now: u64, dry_run: bool) -> Result<GcReport> {
        let mut rep = GcReport::default();
        let tomb_cutoff = now.saturating_sub(self.cfg.blob_retention_ms) as i64;
        let referenced = "EXISTS (SELECT 1 FROM entries e WHERE e.blob = blobs.hash AND (e.purged = 0 OR ifnull(e.purged_at, 0) >= ?1))";
        if dry_run {
            let mut st = self.conn.prepare(&format!(
                "SELECT hash FROM blobs WHERE present = 1 AND NOT {referenced} AND ifnull(unreferenced_since, ?2) <= ?3 AND NOT EXISTS (SELECT 1 FROM uploads u WHERE u.hash = blobs.hash)"
            ))?;
            rep.deleted = st
                .query_map(params![tomb_cutoff, now as i64, tomb_cutoff], |r| {
                    r.get::<_, Vec<u8>>(0)
                })?
                .filter_map(|r| r.ok())
                .filter_map(|b| Hash::from_slice(&b))
                .collect();
            return Ok(rep);
        }
        self.begin()?;
        let head0 = self.head;
        let r = (|| -> Result<()> {
            rep.marked = self.conn.execute(&format!("UPDATE blobs SET unreferenced_since = ?2 WHERE unreferenced_since IS NULL AND NOT {referenced}"), params![tomb_cutoff, now as i64])?;
            self.conn.execute(&format!("UPDATE blobs SET unreferenced_since = NULL WHERE unreferenced_since IS NOT NULL AND {referenced}"), params![tomb_cutoff])?;
            let doomed: Vec<Vec<u8>> = {
                let mut st = self.conn.prepare(
                    "SELECT hash FROM blobs WHERE present = 1 AND unreferenced_since IS NOT NULL AND unreferenced_since <= ?1 AND NOT EXISTS (SELECT 1 FROM uploads u WHERE u.hash = blobs.hash) LIMIT 500",
                )?;
                let v = st
                    .query_map([tomb_cutoff], |r| r.get(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                v
            };
            for h in doomed {
                let seq = self.next_seq();
                self.conn.execute(
                    "UPDATE blobs SET present = 0, seq = ?1 WHERE hash = ?2",
                    params![seq as i64, h],
                )?;
                if let Some(h) = Hash::from_slice(&h) {
                    rep.deleted.push(h);
                }
            }
            let expired: Vec<String> = {
                let mut st = self
                    .conn
                    .prepare("SELECT upload_id FROM uploads WHERE touched_at < ?1")?;
                let v = st
                    .query_map(
                        [now.saturating_sub(self.cfg.upload_expiry_ms) as i64],
                        |r| r.get(0),
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                v
            };
            for u in &expired {
                self.conn
                    .execute("DELETE FROM uploads WHERE upload_id = ?1", [u])?;
            }
            rep.expired_uploads = expired;
            if self.head > head0 {
                self.conn.execute(
                    "INSERT INTO commits(last_seq, first_seq) VALUES (?1, ?2)",
                    params![self.head as i64, (head0 + 1) as i64],
                )?;
                db::set_meta_i64(&self.conn, "head_seq", self.head as i64)?;
            }
            Ok(())
        })();
        match r.and_then(|_| self.commit()) {
            Ok(()) => Ok(rep),
            Err(e) => {
                self.rollback();
                self.head = head0;
                Err(e)
            }
        }
    }

    /// Deletes files of GC'd blobs, re-checking under the writer that each is still absent and
    /// has no upload in progress (so a concurrent re-upload is never deleted).
    pub fn gc_sweep_files(&self, fs: &crate::blobfs::BlobFs, hashes: &[Hash]) -> Result<usize> {
        let mut n = 0;
        for h in hashes {
            let uploading: bool = self.conn.query_row(
                "SELECT count(*) FROM uploads WHERE hash = ?1",
                [h.0.to_vec()],
                |r| r.get::<_, i64>(0),
            )? > 0;
            // A file installed in the last hour may belong to an import that hasn't recorded it yet.
            let fresh = std::fs::metadata(fs.path(h))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|a| a.as_secs() < 3600)
                .unwrap_or(false);
            if !self.blob_present(h)? && !uploading && !fresh {
                fs.delete(h)?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// Server-side correction of blob facts (dimensions etc.) once derived (§3.2).
    pub fn set_blob_info(&mut self, h: Hash, info: &BlobInfo) -> Result<()> {
        self.begin()?;
        let head0 = self.head;
        let seq = self.next_seq();
        let r = self
            .conn
            .execute(
                "UPDATE blobs SET mime = coalesce(?1, mime), width = coalesce(?2, width), height = coalesce(?3, height), orientation = coalesce(?4, orientation), seq = ?5 WHERE hash = ?6",
                params![info.mime, info.width.map(|v| v as i64), info.height.map(|v| v as i64), info.orientation.map(|v| v as i64), seq as i64, h.0.to_vec()],
            )
            .map_err(Error::from)
            .and_then(|_| {
                self.conn.execute("INSERT INTO commits(last_seq, first_seq) VALUES (?1, ?2)", params![self.head as i64, (head0 + 1) as i64])?;
                db::set_meta_i64(&self.conn, "head_seq", self.head as i64).map_err(Error::from)
            });
        match r.and_then(|_| self.commit()) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.rollback();
                self.head = head0;
                Err(e)
            }
        }
    }

    /// Entries rows (for tests / integrity).
    pub fn entry(&self, id: &Id) -> Option<&Entry> {
        self.state.get(id)
    }
}
