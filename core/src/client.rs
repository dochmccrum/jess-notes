//! Sans-IO sync client (DESIGN §5.4). Owns the confirmed state, pending ops, the optimistic view
//! (`confirmed ⊕ pending`, via the same `apply` the server runs), the cursor, quarantine,
//! the redirects overlay and the blob manager.
//!
//! Every call returns an [`Output`]. The host must commit `writes` atomically (in order) before
//! performing `send`, then surface `events`.

use crate::apply::{apply_meta, Ctx, Mode, Tx};
use crate::blobs::{BlobManager, BlobResult, BlobTask};
use crate::hlc::{Clock, Hlc};
use crate::ids::Id;
use crate::kv::{self, Write};
use crate::model::Entry;
use crate::ops::{AckResult, MetaOp, Op, OpBody, Reject};
use crate::proto::{
    Changes, ClientMsg, Hello, ServerMsg, Welcome, MAX_CLIENT_FRAME, PROTO_VERSION,
};
use crate::resolve::Redirects;
use crate::state::MetaState;
use minicbor::{Decode, Encode};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const PING_INTERVAL_MS: u64 = 10_000;
pub const PONG_TIMEOUT_MS: u64 = 5_000;

#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Pending {
    #[n(0)]
    pub op: Op,
    /// Server seq once acknowledged; the op is dropped when the cursor reaches it.
    #[n(1)]
    pub acked: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Quarantined {
    #[n(0)]
    pub op: Op,
    #[n(1)]
    pub reason: Reject,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// These ids changed in the optimistic view.
    EntriesChanged(Vec<Id>),
    /// A remote doc update to merge into an open doc.
    DocRemote {
        entry: Id,
        slot: String,
        update: Vec<u8>,
    },
    /// The server refused an op; it was moved to quarantine.
    Rejected {
        op_id: u64,
        reason: Reject,
    },
    /// Entry purged: local doc rows were deleted.
    Purged(Id),
    /// The connection is dead (pong timeout); the host should reconnect.
    ConnectionDead,
    /// The server refused the session (e.g. wrong vault, revoked token).
    Fatal(String),
    StatusChanged,
}

#[derive(Debug, Default)]
pub struct Output {
    pub writes: Vec<Write>,
    pub send: Vec<ClientMsg>,
    pub events: Vec<Event>,
}

impl Output {
    fn merge(&mut self, o: Output) {
        self.writes.extend(o.writes);
        self.send.extend(o.send);
        self.events.extend(o.events);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conn {
    Offline,
    AwaitWelcome,
    Ready,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Synced,
    Syncing { pending: usize },
    Offline { pending: usize },
    Error(String),
}

pub struct Client {
    pub replica: u64,
    pub vault: Option<Id>,
    next_op: u64,
    pub cursor: u64,
    pub clock: Clock,
    confirmed: MetaState,
    view: MetaState,
    pending: BTreeMap<u64, Pending>,
    /// ids each pending op's prediction touched (for change events on rebase).
    predicted: HashMap<u64, Vec<Id>>,
    in_flight: BTreeSet<u64>,
    docs: HashMap<(Id, String), BTreeSet<u64>>,
    quarantine: BTreeMap<u64, Quarantined>,
    redirects: BTreeMap<(u64, Id), String>,
    redirect_ix: Redirects,
    pub blobs: BlobManager,
    pub conn: Conn,
    pull_outstanding: Option<u64>,
    pub error: Option<String>,
    ping: Option<(u64, u64)>,
    last_ping: u64,
    nonce: u64,
    pub head_seen: u64,
}

impl Client {
    /// A fresh local store.
    pub fn new(replica: u64, chunk: u64) -> (Client, Vec<Write>) {
        let c = Client {
            replica,
            vault: None,
            next_op: 1,
            cursor: 0,
            clock: Clock::new(replica),
            confirmed: MetaState::new(),
            view: MetaState::new(),
            pending: BTreeMap::new(),
            predicted: HashMap::new(),
            in_flight: BTreeSet::new(),
            docs: HashMap::new(),
            quarantine: BTreeMap::new(),
            redirects: BTreeMap::new(),
            redirect_ix: Redirects::default(),
            blobs: BlobManager::new(chunk),
            conn: Conn::Offline,
            pull_outstanding: None,
            error: None,
            ping: None,
            last_ping: 0,
            nonce: 0,
            head_seen: 0,
        };
        let w = vec![
            Write::Put(kv::meta_key("replica"), replica.to_be_bytes().to_vec()),
            Write::Put(kv::meta_key("next_op"), 1u64.to_be_bytes().to_vec()),
        ];
        (c, w)
    }

    /// Rebuilds a client from its persisted key-value pairs. If the store is empty a new replica
    /// is created with `new_replica` (the returned writes must be committed).
    pub fn load(
        items: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
        new_replica: u64,
        chunk: u64,
    ) -> (Client, Vec<Write>) {
        let items: Vec<(Vec<u8>, Vec<u8>)> = items.into_iter().collect();
        let replica = items
            .iter()
            .find(|(k, _)| *k == kv::meta_key("replica"))
            .and_then(|(_, v)| v.as_slice().try_into().ok())
            .map(u64::from_be_bytes);
        let (mut c, w) = match replica {
            Some(r) => (Client::new(r, chunk).0, vec![]),
            None => Client::new(new_replica, chunk),
        };
        let mut entries = Vec::new();
        for (k, v) in &items {
            match k.first().copied() {
                Some(kv::P_META) => {
                    let name = std::str::from_utf8(&k[1..]).unwrap_or("");
                    let u = v.as_slice().try_into().ok().map(u64::from_be_bytes);
                    match name {
                        "next_op" => c.next_op = u.unwrap_or(1),
                        "cursor" => c.cursor = u.unwrap_or(0),
                        "vault" => c.vault = Id::from_slice(v),
                        "hlc" => {
                            if let Ok(h) = minicbor::decode::<Hlc>(v) {
                                c.clock.observe(h);
                            }
                        }
                        _ => {}
                    }
                }
                Some(kv::P_ENTRY) => {
                    if let Ok(e) = minicbor::decode::<Entry>(v) {
                        entries.push(e);
                    }
                }
                Some(kv::P_PENDING) => {
                    if let Ok(p) = minicbor::decode::<Pending>(v) {
                        c.pending.insert(p.op.op_id, p);
                    }
                }
                Some(kv::P_QUARANTINE) => {
                    if let Ok(q) = minicbor::decode::<Quarantined>(v) {
                        c.quarantine.insert(q.op.op_id, q);
                    }
                }
                Some(kv::P_DOC) => {
                    if let Some((id, slot, seq)) = kv::parse_doc_key(k) {
                        c.docs.entry((id, slot)).or_default().insert(seq);
                    }
                }
                Some(kv::P_REDIRECT) if k.len() == 25 => {
                    let op = u64::from_be_bytes(k[1..9].try_into().expect("8"));
                    if let (Some(id), Ok(p)) = (Id::from_slice(&k[9..25]), std::str::from_utf8(v)) {
                        c.redirects.insert((op, id), p.to_string());
                    }
                }
                Some(kv::P_BLOB) => c.blobs.load_blob(v),
                Some(kv::P_DOWNLOAD) => c.blobs.load_download(k, v),
                _ => {}
            }
        }
        for e in &entries {
            for h in [e.clock.parent, e.clock.name, e.clock.trashed] {
                c.clock.observe(h);
            }
        }
        c.confirmed = MetaState::from_entries(entries);
        c.rebuild_redirects();
        c.rebuild_view();
        (c, w)
    }

    // ---------------------------------------------------------------- accessors

    pub fn view(&self) -> &MetaState {
        &self.view
    }
    pub fn confirmed(&self) -> &MetaState {
        &self.confirmed
    }
    pub fn redirects(&self) -> &Redirects {
        &self.redirect_ix
    }
    pub fn pending_count(&self) -> usize {
        self.pending.values().filter(|p| p.acked.is_none()).count()
    }
    pub fn pending_ops(&self) -> impl Iterator<Item = &Pending> {
        self.pending.values()
    }
    pub fn quarantine(&self) -> impl Iterator<Item = &Quarantined> {
        self.quarantine.values()
    }
    /// Confirmed doc rows (seqs) for a doc; the host reads their bytes at `kv::doc_key`.
    pub fn doc_seqs(&self, entry: Id, slot: &str) -> Vec<u64> {
        self.docs
            .get(&(entry, slot.to_string()))
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }
    /// Local (unconfirmed) updates of a doc, in order.
    pub fn pending_doc_updates(&self, entry: Id, slot: &str) -> Vec<Vec<u8>> {
        self.pending
            .values()
            .filter_map(|p| match &p.op.body {
                OpBody::Doc {
                    entry: e,
                    slot: s,
                    update,
                } if *e == entry && s == slot => Some(update.clone()),
                _ => None,
            })
            .collect()
    }
    pub fn docs(&self) -> impl Iterator<Item = &(Id, String)> {
        self.docs.keys()
    }
    pub fn status(&self) -> Status {
        if let Some(e) = &self.error {
            return Status::Error(e.clone());
        }
        let n = self.pending_count();
        match self.conn {
            Conn::Ready if n == 0 && self.cursor >= self.head_seen => Status::Synced,
            Conn::Ready => Status::Syncing { pending: n },
            _ => Status::Offline { pending: n },
        }
    }
    /// All caught up with the server and nothing pending (used by tests).
    pub fn is_quiescent(&self, head: u64) -> bool {
        self.pending.is_empty() && self.cursor == head && self.blobs.unconfirmed().is_empty()
    }

    // ---------------------------------------------------------------- internals

    fn put_meta_u64(&self, name: &str, v: u64) -> Write {
        Write::Put(kv::meta_key(name), v.to_be_bytes().to_vec())
    }

    fn rebuild_redirects(&mut self) {
        let mut r = Redirects::default();
        for ((_, id), p) in &self.redirects {
            r.add(p, *id);
        }
        self.redirect_ix = r;
    }

    /// view = confirmed ⊕ pending. Returns ids whose view may have changed.
    fn rebuild_view(&mut self) -> BTreeSet<Id> {
        let mut changed: BTreeSet<Id> = self.predicted.values().flatten().copied().collect();
        self.predicted.clear();
        let mut view = self.confirmed.clone();
        for p in self.pending.values() {
            if let OpBody::Meta(m) = &p.op.body {
                let ctx = Ctx {
                    hlc: p.op.hlc,
                    known_seq: p.op.known_seq,
                    replica: self.replica,
                    op_id: p.op.op_id,
                    mode: Mode::Predict,
                    now: p.op.hlc.wall,
                };
                let mut tx = Tx::new();
                if apply_meta(&mut view, &mut tx, m, &ctx).is_ok() {
                    let ids = tx.changed(&view);
                    changed.extend(ids.iter().copied());
                    self.predicted.insert(p.op.op_id, ids);
                }
            }
        }
        self.view = view;
        changed
    }

    fn has_pending_meta(&self) -> bool {
        self.pending
            .values()
            .any(|p| matches!(p.op.body, OpBody::Meta(_)))
    }

    fn next_op_id(&mut self, w: &mut Vec<Write>) -> u64 {
        let id = self.next_op;
        self.next_op += 1;
        w.push(self.put_meta_u64("next_op", self.next_op));
        id
    }

    fn flush(&mut self, out: &mut Output) {
        if self.conn != Conn::Ready {
            return;
        }
        let mut frame: Vec<Op> = Vec::new();
        let mut size = 0usize;
        let mut last_group = 0u64;
        let unsent: Vec<Op> = self
            .pending
            .values()
            .filter(|p| p.acked.is_none() && !self.in_flight.contains(&p.op.op_id))
            .map(|p| p.op.clone())
            .collect();
        for op in unsent {
            let s = minicbor::to_vec(&op).map(|v| v.len()).unwrap_or(0) + 16;
            let same_group = op.group != 0 && op.group == last_group;
            if !frame.is_empty() && size + s > MAX_CLIENT_FRAME && !same_group {
                out.send.push(ClientMsg::Push {
                    ops: std::mem::take(&mut frame),
                });
                size = 0;
            }
            last_group = op.group;
            size += s;
            self.in_flight.insert(op.op_id);
            frame.push(op);
        }
        if !frame.is_empty() {
            out.send.push(ClientMsg::Push { ops: frame });
        }
    }

    fn drop_pending(&mut self, op_id: u64, out: &mut Output) {
        self.pending.remove(&op_id);
        self.in_flight.remove(&op_id);
        out.writes.push(Write::Del(kv::pending_key(op_id)));
        let keys: Vec<(u64, Id)> = self
            .redirects
            .range((op_id, Id([0; 16]))..=(op_id, Id([0xff; 16])))
            .map(|(k, _)| *k)
            .collect();
        if !keys.is_empty() {
            for k in keys {
                self.redirects.remove(&k);
                out.writes.push(Write::Del(kv::redirect_key(k.0, k.1)));
            }
            self.rebuild_redirects();
        }
    }

    // ---------------------------------------------------------------- connection

    /// The transport connected: send `Hello`.
    pub fn connected(&mut self, token: &str, app_version: &str) -> Output {
        self.conn = Conn::AwaitWelcome;
        self.in_flight.clear();
        self.pull_outstanding = None;
        self.ping = None;
        self.blobs.reset_in_flight();
        Output {
            send: vec![ClientMsg::Hello(Hello {
                proto: PROTO_VERSION,
                vault_id: self.vault,
                replica_id: self.replica,
                token: token.to_string(),
                cursor: self.cursor,
                app_version: app_version.to_string(),
            })],
            events: vec![Event::StatusChanged],
            ..Default::default()
        }
    }

    pub fn disconnected(&mut self) -> Output {
        self.conn = Conn::Offline;
        self.in_flight.clear();
        self.pull_outstanding = None;
        self.ping = None;
        self.blobs.reset_in_flight();
        Output {
            events: vec![Event::StatusChanged],
            ..Default::default()
        }
    }

    /// Periodic timer: heartbeats (§5.5).
    pub fn tick(&mut self, now: u64) -> Output {
        let mut out = Output::default();
        if self.conn != Conn::Ready {
            return out;
        }
        if let Some((_, sent)) = self.ping {
            if now.saturating_sub(sent) > PONG_TIMEOUT_MS {
                out.events.push(Event::ConnectionDead);
                out.merge(self.disconnected());
            }
            return out;
        }
        if now.saturating_sub(self.last_ping) >= PING_INTERVAL_MS {
            out.send.push(self.probe(now));
        }
        out
    }

    /// An immediate liveness probe (foreground, `online` event, network change).
    pub fn probe(&mut self, now: u64) -> ClientMsg {
        self.nonce += 1;
        self.ping = Some((self.nonce, now));
        self.last_ping = now;
        ClientMsg::Ping { nonce: self.nonce }
    }

    pub fn on_message(&mut self, msg: ServerMsg, now: u64) -> Output {
        let _ = now;
        match msg {
            ServerMsg::Welcome(w) => self.on_welcome(w),
            ServerMsg::Ack { results } => self.on_ack(results),
            ServerMsg::Changes(c) => self.on_changes(c),
            ServerMsg::Pong { nonce, .. } => {
                if self.ping.map(|p| p.0) == Some(nonce) {
                    self.ping = None;
                }
                Output::default()
            }
            ServerMsg::Error { code, message, .. } => {
                use crate::proto::ErrorCode::*;
                let fatal = matches!(code, Unauthorized | WrongVault | ProtoTooOld | Revoked);
                let mut out = self.disconnected();
                if fatal {
                    self.error = Some(format!("{code:?}: {message}"));
                    out.events.push(Event::Fatal(message));
                }
                out
            }
            ServerMsg::DocDiff {
                entry,
                slot,
                update,
            } => Output {
                events: vec![Event::DocRemote {
                    entry,
                    slot,
                    update,
                }],
                ..Default::default()
            },
        }
    }

    fn on_welcome(&mut self, w: Welcome) -> Output {
        let mut out = Output::default();
        if let Some(v) = self.vault {
            if v != w.vault_id {
                self.error = Some("this local store belongs to a different vault".into());
                out.events.push(Event::Fatal("wrong vault".into()));
                out.merge(self.disconnected());
                return out;
            }
        } else {
            self.vault = Some(w.vault_id);
            out.writes
                .push(Write::Put(kv::meta_key("vault"), w.vault_id.0.to_vec()));
        }
        if w.head_seq < self.cursor {
            self.error = Some(format!("server is behind this device (server seq {} < local {}); was it restored from a snapshot?", w.head_seq, self.cursor));
            out.events.push(Event::StatusChanged);
            return out;
        }
        self.error = None;
        self.head_seen = self.head_seen.max(w.head_seq);
        self.clock.observe(w.hlc);
        self.conn = Conn::Ready;
        self.flush(&mut out);
        out.events.push(Event::StatusChanged);
        out
    }

    fn on_ack(&mut self, results: Vec<(u64, AckResult)>) -> Output {
        let mut out = Output::default();
        let mut rebuild = false;
        for (op_id, r) in results {
            self.in_flight.remove(&op_id);
            let Some(p) = self.pending.get(&op_id).cloned() else {
                continue;
            };
            match r {
                AckResult::Applied(seq) | AckResult::Duplicate(seq) => match &p.op.body {
                    OpBody::Doc {
                        entry,
                        slot,
                        update,
                    } => {
                        // The update is now part of the confirmed doc. An unknown seq (the
                        // server pruned the record) gets a unique key that sorts last.
                        let seq = if seq == 0 { u64::MAX - op_id } else { seq };
                        // `Applied` always joins the doc (the server may have recovered a purged
                        // note). A `Duplicate` for a purged doc carries nothing to keep.
                        // Purged after this update was applied (the purge row has a later seq)?
                        let raw = match r {
                            AckResult::Applied(s) | AckResult::Duplicate(s) => s,
                            _ => 0,
                        };
                        let purged = self
                            .confirmed
                            .get(entry)
                            .map(|e| e.purged && (raw == 0 || e.seq >= raw))
                            .unwrap_or(false);
                        // A row already received for this seq (e.g. a recovered note's merged
                        // state) is a superset: never overwrite it with the bare update.
                        let have = self
                            .docs
                            .get(&(*entry, slot.clone()))
                            .map(|s| s.contains(&seq))
                            .unwrap_or(false);
                        if !have && !purged {
                            out.writes
                                .push(Write::Put(kv::doc_key(*entry, slot, seq), update.clone()));
                            self.docs
                                .entry((*entry, slot.clone()))
                                .or_default()
                                .insert(seq);
                        }
                        self.drop_pending(op_id, &mut out);
                    }
                    OpBody::Meta(_) => {
                        if seq <= self.cursor {
                            self.drop_pending(op_id, &mut out);
                            rebuild = true;
                        } else {
                            let np = Pending {
                                op: p.op.clone(),
                                acked: Some(seq),
                            };
                            out.writes.push(Write::Put(
                                kv::pending_key(op_id),
                                minicbor::to_vec(&np).expect("encode"),
                            ));
                            self.pending.insert(op_id, np);
                        }
                    }
                },
                AckResult::Rejected(reason) => {
                    let q = Quarantined {
                        op: p.op.clone(),
                        reason,
                    };
                    out.writes.push(Write::Put(
                        kv::quarantine_key(op_id),
                        minicbor::to_vec(&q).expect("encode"),
                    ));
                    self.quarantine.insert(op_id, q);
                    self.drop_pending(op_id, &mut out);
                    out.events.push(Event::Rejected { op_id, reason });
                    rebuild = true;
                }
            }
        }
        if rebuild {
            let ids = self.rebuild_view();
            out.events
                .push(Event::EntriesChanged(ids.into_iter().collect()));
        }
        out.events.push(Event::StatusChanged);
        out
    }

    fn on_changes(&mut self, c: Changes) -> Output {
        let mut out = Output::default();
        self.head_seen = self.head_seen.max(c.to);
        if c.to <= self.cursor {
            return out;
        }
        if c.from != self.cursor {
            if self.pull_outstanding != Some(self.cursor) {
                self.pull_outstanding = Some(self.cursor);
                out.send.push(ClientMsg::Pull {
                    from: self.cursor,
                    limit_bytes: crate::proto::CHANGES_PAGE_BYTES,
                });
            }
            return out;
        }
        self.pull_outstanding = None;
        self.clock.observe(c.hlc);
        let mut changed: BTreeSet<Id> = BTreeSet::new();
        // With pending meta ops the view is rebuilt from confirmed ⊕ pending afterwards.
        let rebuild = self.has_pending_meta() || !self.predicted.is_empty();
        for e in c.entries {
            if let Some(cur) = self.confirmed.get(&e.id) {
                if cur.seq >= e.seq {
                    continue;
                }
            }
            for h in [
                e.clock.parent,
                e.clock.name,
                e.clock.trashed,
                e.clock.visible,
                e.clock.blob,
            ] {
                self.clock.observe(h);
            }
            if e.purged {
                let key_prefix: Vec<(Id, String)> = self
                    .docs
                    .keys()
                    .filter(|(id, _)| *id == e.id)
                    .cloned()
                    .collect();
                // Only rows up to the purge: later rows belong to a recovery (§6.4).
                for k in key_prefix {
                    if let Some(seqs) = self.docs.get_mut(&k) {
                        let old: Vec<u64> = seqs
                            .iter()
                            .copied()
                            .filter(|s| *s <= e.seq || *s > u64::MAX / 2)
                            .collect();
                        for s in old {
                            seqs.remove(&s);
                            out.writes.push(Write::Del(kv::doc_key(k.0, &k.1, s)));
                        }
                        if seqs.is_empty() {
                            self.docs.remove(&k);
                        }
                    }
                }
                out.events.push(Event::Purged(e.id));
            }
            out.writes.push(Write::Put(
                kv::entry_key(e.id),
                minicbor::to_vec(&e).expect("encode"),
            ));
            changed.insert(e.id);
            if !rebuild {
                self.view.put(e.clone());
            }
            self.confirmed.put(e);
        }
        for b in c.blobs {
            if b.present {
                out.writes.extend(self.blobs.server_present(b.hash));
            } else {
                out.writes.extend(self.blobs.server_absent(b.hash));
            }
        }
        for d in c.docs {
            if let Some(bytes) = d.bytes {
                out.writes.push(Write::Put(
                    kv::doc_key(d.entry, &d.slot, d.seq),
                    bytes.clone(),
                ));
                self.docs
                    .entry((d.entry, d.slot.clone()))
                    .or_default()
                    .insert(d.seq);
                out.events.push(Event::DocRemote {
                    entry: d.entry,
                    slot: d.slot,
                    update: bytes,
                });
            }
        }
        self.cursor = c.to;
        out.writes.push(self.put_meta_u64("cursor", self.cursor));
        out.writes.push(Write::Put(
            kv::meta_key("hlc"),
            minicbor::to_vec(self.clock.last).expect("encode"),
        ));
        let done: Vec<u64> = self
            .pending
            .values()
            .filter(|p| p.acked.map(|s| s <= self.cursor).unwrap_or(false))
            .map(|p| p.op.op_id)
            .collect();
        for op in done {
            self.drop_pending(op, &mut out);
        }
        if rebuild {
            changed.extend(self.rebuild_view());
        }
        if !changed.is_empty() {
            out.events
                .push(Event::EntriesChanged(changed.into_iter().collect()));
        }
        if c.more {
            // The server keeps streaming pages; nothing to do.
        }
        out.events.push(Event::StatusChanged);
        out
    }

    // ---------------------------------------------------------------- local intents

    /// Applies meta intents optimistically and queues them. Multiple ops form one atomic group.
    /// A prediction failure (e.g. a cycle) returns `Err` and queues nothing.
    pub fn local_meta(&mut self, ops: Vec<MetaOp>, now: u64) -> Result<Output, Reject> {
        let mut out = Output::default();
        let first = self.next_op;
        let group = if ops.len() > 1 { first } else { 0 };
        let mut tx = Tx::new();
        let mut built = Vec::new();
        for (i, m) in ops.into_iter().enumerate() {
            let op_id = first + i as u64;
            let hlc = self.clock.now(now);
            let ctx = Ctx {
                hlc,
                known_seq: self.cursor,
                replica: self.replica,
                op_id,
                mode: Mode::Predict,
                now,
            };
            let mut optx = Tx::new();
            if let Err(e) = apply_meta(&mut self.view, &mut optx, &m, &ctx) {
                optx.rollback(&mut self.view);
                tx.rollback(&mut self.view);
                return Err(e);
            }
            self.predicted.insert(op_id, optx.changed(&self.view));
            tx.absorb(optx);
            built.push(Op {
                op_id,
                hlc,
                known_seq: self.cursor,
                group,
                body: OpBody::Meta(m),
            });
        }
        for op in &built {
            let n = self.next_op_id(&mut out.writes);
            debug_assert_eq!(n, op.op_id);
            let p = Pending {
                op: op.clone(),
                acked: None,
            };
            out.writes.push(Write::Put(
                kv::pending_key(op.op_id),
                minicbor::to_vec(&p).expect("encode"),
            ));
            self.pending.insert(op.op_id, p);
        }
        // Redirects overlay for renamed/moved entries (and everything under moved folders).
        let last_op = first + built.len() as u64 - 1;
        let mut added = false;
        for r in &tx.renames {
            let mut pairs = vec![(r.entry, r.old_path.clone())];
            if r.is_folder {
                for d in self.view.descendants(r.entry) {
                    if let Some(np) = self.view.path_of(d) {
                        if let Some(rest) = np.strip_prefix(&r.new_path) {
                            pairs.push((d, format!("{}{}", r.old_path, rest)));
                        }
                    }
                }
            }
            for (id, old) in pairs {
                if self.view.get(&id).map(|e| e.is_linkable()).unwrap_or(false)
                    && self.confirmed.get(&id).is_some()
                {
                    out.writes.push(Write::Put(
                        kv::redirect_key(last_op, id),
                        old.as_bytes().to_vec(),
                    ));
                    self.redirects.insert((last_op, id), old);
                    added = true;
                }
            }
        }
        if added {
            self.rebuild_redirects();
        }
        out.events
            .push(Event::EntriesChanged(tx.changed(&self.view)));
        out.events.push(Event::StatusChanged);
        self.flush(&mut out);
        Ok(out)
    }

    /// Queues a local Yjs update (already coalesced and applied to the local doc by the host).
    pub fn local_doc_update(&mut self, entry: Id, slot: &str, update: Vec<u8>, now: u64) -> Output {
        let mut out = Output::default();
        let op_id = self.next_op_id(&mut out.writes);
        let hlc = self.clock.now(now);
        let op = Op {
            op_id,
            hlc,
            known_seq: self.cursor,
            group: 0,
            body: OpBody::Doc {
                entry,
                slot: slot.to_string(),
                update,
            },
        };
        let p = Pending { op, acked: None };
        out.writes.push(Write::Put(
            kv::pending_key(op_id),
            minicbor::to_vec(&p).expect("encode"),
        ));
        self.pending.insert(op_id, p);
        out.events.push(Event::StatusChanged);
        self.flush(&mut out);
        out
    }

    /// Replaces confirmed rows `≤ upto` of a doc with one merged row at `upto` (client compaction).
    pub fn compact_doc(&mut self, entry: Id, slot: &str, merged: Vec<u8>, upto: u64) -> Vec<Write> {
        let mut w = Vec::new();
        let key = (entry, slot.to_string());
        if let Some(seqs) = self.docs.get_mut(&key) {
            let old: Vec<u64> = seqs.range(..=upto).copied().collect();
            for s in &old {
                seqs.remove(s);
                w.push(Write::Del(kv::doc_key(entry, slot, *s)));
            }
            seqs.insert(upto);
            w.push(Write::Put(kv::doc_key(entry, slot, upto), merged));
        }
        w
    }

    /// Re-queues a quarantined op (user chose "retry").
    pub fn retry_quarantined(&mut self, op_id: u64, now: u64) -> Option<Output> {
        let q = self.quarantine.remove(&op_id)?;
        let mut out = Output::default();
        out.writes.push(Write::Del(kv::quarantine_key(op_id)));
        match q.op.body.clone() {
            OpBody::Meta(m) => match self.local_meta(vec![m], now) {
                Ok(o) => out.merge(o),
                Err(_) => {
                    self.quarantine.insert(
                        op_id,
                        Quarantined {
                            op: q.op.clone(),
                            reason: q.reason,
                        },
                    );
                    return None;
                }
            },
            OpBody::Doc {
                entry,
                slot,
                update,
            } => out.merge(self.local_doc_update(entry, &slot, update, now)),
        }
        Some(out)
    }

    // ---------------------------------------------------------------- blobs

    pub fn blob_tasks(&mut self) -> Vec<BlobTask> {
        if self.conn != Conn::Ready {
            return vec![];
        }
        self.blobs.next_tasks()
    }

    pub fn blob_result(&mut self, r: BlobResult, now: u64) -> Vec<Write> {
        self.blobs.on_result(r, now)
    }
}

/// Reconnect backoff (§5.5): immediate, 250 ms, 500 ms, 1 s, 2 s, 4 s, 8 s, 15 s cap
/// (60 s in the background), ±20 % jitter. `jitter` ∈ [0, 1).
pub fn reconnect_delay_ms(attempt: u32, foreground: bool, jitter: f64) -> u64 {
    const STEPS: [u64; 8] = [0, 250, 500, 1000, 2000, 4000, 8000, 15000];
    let base = if (attempt as usize) < STEPS.len() {
        STEPS[attempt as usize]
    } else if foreground {
        15_000
    } else {
        (15_000u64 << (attempt as usize - STEPS.len() + 1).min(2)).min(60_000)
    };
    let base = if foreground {
        base.min(15_000)
    } else {
        base.min(60_000)
    };
    let j = 0.8 + 0.4 * jitter.clamp(0.0, 1.0);
    (base as f64 * j) as u64
}
