//! Deterministic simulation (DESIGN §17.1): the real engine on SQLite, N real `core` sync clients
//! (yrs docs), a simulated network (drops via disconnects, duplication, reordering, delay,
//! partitions), client and server crashes, skewed clocks, and chunked blob transfers with
//! interrupted / duplicated / resumed chunks. After quiescing, every invariant is asserted.
//!
//! `SIM_SEEDS=n` (default 200) seeds from `SIM_START` (default 0); `SIM_SEED=k` runs one seed
//! with a full trace.

use jess_core::blobs::{Bitmap, BlobResult, BlobTask};
use jess_core::client::{Client, Output};
use jess_core::doc as ydoc;
use jess_core::kv;
use jess_core::links::extract;
use jess_core::model::{BlobInfo, KIND_FOLDER, KIND_MARKDOWN, KIND_MEDIA, KIND_PDF, SLOT_BODY};
use jess_core::names::RECOVERED_PREFIX;
use jess_core::ops::{AckResult, MetaOp, OpBody};
use jess_core::proto::{ClientMsg, ServerMsg, Welcome};
use jess_core::{Hash, Id};
use jess_server::blobfs::BlobFs;
use jess_server::changes::read_changes;
use jess_server::db;
use jess_server::engine::{BeginUpload, Engine, EngineConfig};
use std::collections::{BTreeMap, HashMap, HashSet};

const CHUNK: u64 = 16;
const DAY: u64 = 86_400_000;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
    fn chance(&mut self, p: f64) -> bool {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> Option<&'a T> {
        if v.is_empty() {
            None
        } else {
            Some(&v[self.below(v.len() as u64) as usize])
        }
    }
}

/// Docs edited in the push, and each other doc's link resolutions before it.
type RPre = (HashSet<Id>, HashMap<Id, Vec<Option<Id>>>);

struct Envelope<T> {
    at: u64,
    msg: T,
}

struct SimClient {
    c: Client,
    kv: BTreeMap<Vec<u8>, Vec<u8>>,
    skew: i64,
    connected: bool,
    to_server: Vec<Envelope<ClientMsg>>,
    to_client: Vec<Envelope<ServerMsg>>,
    /// Local blob storage (survives crashes, like files on disk).
    blob_bytes: HashMap<Hash, Vec<u8>>,
    created_blobs: HashSet<Hash>,
    open_docs: HashMap<Id, yrs::Doc>,
    sent: HashMap<u64, OpBody>,
    doc_client_seed: u64,
}

struct ServerConn {
    replica: u64,
    cursor: u64,
}

struct Faults {
    dup: f64,
    reorder: f64,
    disconnect: f64,
    client_crash: f64,
    server_crash: f64,
    blob_fail: f64,
}

struct Sim {
    rng: Rng,
    now: u64,
    engine: Option<Engine>,
    fs: BlobFs,
    _dir: tempfile::TempDir,
    clients: Vec<SimClient>,
    conns: HashMap<usize, ServerConn>,
    trace: Vec<String>,
    verbose: bool,
    acked_docs: Vec<(Id, String, Vec<u8>)>,
    faults: Faults,
    page_limit: u64,
    seed: u64,
}

const NAMES: &[&str] = &[
    "a.md",
    "b.md",
    "Note.md",
    "note.md",
    "x y.md",
    "Ünï.md",
    "Untitled.md",
];
const FOLDERS: &[&str] = &["F", "G", "sub dir"];

fn engine_cfg() -> EngineConfig {
    EngineConfig {
        trash_retention_ms: 10 * 60_000,
        blob_retention_ms: 20 * 60_000,
        chunk_size: CHUNK,
        compact_rows: 6,
        compact_age_ms: 5 * 60_000,
        upload_expiry_ms: 60 * 60_000,
        doc_cache: 8,
        ..Default::default()
    }
}

impl Sim {
    fn new(seed: u64) -> Sim {
        let mut rng = Rng(seed.wrapping_mul(0x2545F4914F6CDD1D) ^ 0xABCDEF);
        let dir = tempfile::tempdir().unwrap();
        let fs = BlobFs::new(dir.path().join("blobs"), false).unwrap();
        let conn = db::open_memory().unwrap();
        let engine = Engine::open(conn, engine_cfg(), seed).unwrap();
        let n = 2 + rng.below(3) as usize;
        let mut clients = Vec::new();
        for i in 0..n {
            let (c, w) = Client::new(1000 + i as u64 * 7 + rng.below(1 << 40), CHUNK);
            let mut kvs = BTreeMap::new();
            apply_writes(&mut kvs, w);
            let skew = (rng.below(6 * DAY) as i64) - 3 * DAY as i64;
            clients.push(SimClient {
                c,
                kv: kvs,
                skew,
                connected: false,
                to_server: vec![],
                to_client: vec![],
                blob_bytes: HashMap::new(),
                created_blobs: HashSet::new(),
                open_docs: HashMap::new(),
                sent: HashMap::new(),
                doc_client_seed: rng.next(),
            });
        }
        let intensity = rng.below(3);
        let f = |base: f64| base * [0.3, 1.0, 2.5][intensity as usize];
        let faults = Faults {
            dup: f(0.03),
            reorder: f(0.05),
            disconnect: f(0.02),
            client_crash: f(0.01),
            server_crash: f(0.01),
            blob_fail: f(0.1),
        };
        let page_limit = [64u64, 512, 1 << 20][rng.below(3) as usize];
        Sim {
            rng,
            now: 10 * DAY,
            engine: Some(engine),
            fs,
            _dir: dir,
            clients,
            conns: HashMap::new(),
            trace: vec![],
            verbose: false,
            acked_docs: vec![],
            faults,
            page_limit,
            seed,
        }
    }

    fn engine(&mut self) -> &mut Engine {
        self.engine.as_mut().unwrap()
    }

    fn log(&mut self, s: String) {
        if self.verbose {
            eprintln!("[{}] {}", self.now, s);
        }
        self.trace.push(s);
        if self.trace.len() > 400 {
            self.trace.drain(..100);
        }
    }

    fn cnow(&self, ci: usize) -> u64 {
        (self.now as i64 + self.clients[ci].skew).max(1) as u64
    }

    // ------------------------------------------------------------------ client plumbing

    fn handle_output(&mut self, ci: usize, out: Output, may_crash: bool) {
        if may_crash && self.rng.chance(self.faults.client_crash) {
            self.log(format!(
                "c{ci} CRASH before commit ({} writes lost)",
                out.writes.len()
            ));
            self.crash_client(ci);
            return;
        }
        let cl = &mut self.clients[ci];
        apply_writes(&mut cl.kv, out.writes);
        for m in out.send {
            if let ClientMsg::Push { ops } = &m {
                for op in ops {
                    cl.sent.insert(op.op_id, op.body.clone());
                }
            }
            if cl.connected {
                let d = self.rng.below(30);
                cl.to_server.push(Envelope {
                    at: self.now + d,
                    msg: m,
                });
            }
        }
        for e in out.events {
            match e {
                jess_core::client::Event::DocRemote {
                    entry,
                    slot,
                    update,
                } if slot == SLOT_BODY => {
                    if let Some(d) = self.clients[ci].open_docs.get(&entry) {
                        let _ = ydoc::apply(d, &update);
                    }
                }
                jess_core::client::Event::Purged(id) => {
                    self.clients[ci].open_docs.remove(&id);
                }
                jess_core::client::Event::ConnectionDead => {
                    self.disconnect(ci);
                }
                _ => {}
            }
        }
    }

    fn crash_client(&mut self, ci: usize) {
        self.disconnect(ci);
        let cl = &mut self.clients[ci];
        let items: Vec<(Vec<u8>, Vec<u8>)> =
            cl.kv.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let (c, w) = Client::load(items, 1, CHUNK);
        assert!(w.is_empty());
        cl.c = c;
        cl.open_docs.clear();
    }

    fn disconnect(&mut self, ci: usize) {
        let cl = &mut self.clients[ci];
        if !cl.connected {
            return;
        }
        cl.connected = false;
        cl.to_server.clear();
        cl.to_client.clear();
        let o = cl.c.disconnected();
        apply_writes(&mut cl.kv, o.writes);
        self.conns.remove(&ci);
        self.log(format!("c{ci} disconnected"));
    }

    fn connect(&mut self, ci: usize) {
        if self.clients[ci].connected {
            return;
        }
        self.clients[ci].connected = true;
        let o = self.clients[ci].c.connected("token", "sim");
        self.log(format!(
            "c{ci} connected (cursor {})",
            self.clients[ci].c.cursor
        ));
        self.handle_output(ci, o, false);
    }

    fn open_doc(&mut self, ci: usize, id: Id) -> &yrs::Doc {
        if !self.clients[ci].open_docs.contains_key(&id) {
            let seed = self.clients[ci]
                .doc_client_seed
                .wrapping_add(self.rng.next());
            let cl = &self.clients[ci];
            let d = ydoc::new_doc((seed & ((1 << 53) - 1)).max(1));
            let prefix = kv::doc_prefix(id, SLOT_BODY);
            for (_, v) in cl
                .kv
                .range(prefix.clone()..)
                .take_while(|(k, _)| k.starts_with(&prefix))
            {
                ydoc::apply(&d, v).unwrap();
            }
            for u in cl.c.pending_doc_updates(id, SLOT_BODY) {
                ydoc::apply(&d, &u).unwrap();
            }
            self.clients[ci].open_docs.insert(id, d);
        }
        &self.clients[ci].open_docs[&id]
    }

    // ------------------------------------------------------------------ server plumbing

    fn server_send(&mut self, ci: usize, m: ServerMsg) {
        let d = self.rng.below(30);
        let at = self.now + d;
        self.clients[ci].to_client.push(Envelope { at, msg: m });
    }

    fn pump(&mut self, ci: usize) {
        loop {
            let Some(conn) = self.conns.get(&ci) else {
                return;
            };
            let (cursor, replica) = (conn.cursor, conn.replica);
            let limit = self.page_limit;
            let e = self.engine.as_ref().unwrap();
            let head = e.head;
            if cursor >= head {
                return;
            }
            let c = read_changes(&e.conn, cursor, head, replica, limit, e.hlc()).unwrap();
            assert!(c.to > cursor, "changes must progress");
            self.conns.get_mut(&ci).unwrap().cursor = c.to;
            self.server_send(ci, ServerMsg::Changes(c));
        }
    }

    fn pump_all(&mut self) {
        let mut ids: Vec<usize> = self.conns.keys().copied().collect();
        ids.sort();
        for ci in ids {
            self.pump(ci);
        }
    }

    fn server_crash(&mut self) {
        self.log("SERVER RESTART".into());
        let e = self.engine.take().unwrap();
        let conn = e.into_conn();
        self.engine = Some(Engine::open(conn, engine_cfg(), self.rng.next()).unwrap());
        for ci in 0..self.clients.len() {
            self.disconnect(ci);
        }
    }

    fn server_recv(&mut self, ci: usize, m: ClientMsg) {
        if !self.conns.contains_key(&ci) && !matches!(m, ClientMsg::Hello(_)) {
            return;
        }
        match m {
            ClientMsg::Hello(h) => {
                let e = self.engine.as_ref().unwrap();
                if let Some(v) = h.vault_id {
                    assert_eq!(v, e.vault_id);
                }
                let w = Welcome {
                    vault_id: e.vault_id,
                    head_seq: e.head,
                    server_time: self.now,
                    min_client_proto: 1,
                    hlc: e.hlc(),
                };
                self.conns.insert(
                    ci,
                    ServerConn {
                        replica: h.replica_id,
                        cursor: h.cursor,
                    },
                );
                self.server_send(ci, ServerMsg::Welcome(w));
                self.pump(ci);
            }
            ClientMsg::Push { ops } => {
                let replica = self.conns[&ci].replica;
                if self.rng.chance(self.faults.server_crash) {
                    self.engine().fail_next_commit = true;
                }
                let pre = self.invariant_r_pre(&ops);
                let purged_before: HashSet<Id> = self
                    .engine()
                    .state
                    .iter()
                    .filter(|e| e.purged)
                    .map(|e| e.id)
                    .collect();
                let now = self.now;
                match self.engine().push(replica, None, &ops, now) {
                    Ok(acks) => {
                        if self.verbose {
                            let d: Vec<String> = ops
                                .iter()
                                .map(|o| match &o.body {
                                    OpBody::Meta(m) => format!("{m:?}"),
                                    OpBody::Doc { entry, .. } => format!("doc {entry:?}"),
                                })
                                .collect();
                            self.log(format!("   ops {d:?}"));
                        }
                        self.log(format!(
                            "c{ci} push {} ops -> {:?}",
                            ops.len(),
                            acks.iter().map(|a| a.1).collect::<Vec<_>>()
                        ));
                        for (op_id, r) in &acks {
                            if let AckResult::Applied(_) | AckResult::Duplicate(_) = r {
                                if let Some(OpBody::Doc {
                                    entry,
                                    slot,
                                    update,
                                }) = self.clients[ci].sent.get(op_id)
                                {
                                    self.acked_docs.push((*entry, slot.clone(), update.clone()));
                                }
                            }
                        }
                        self.check_resurrection(&purged_before);
                        if let Some(pre) = pre {
                            self.invariant_r_post(pre);
                        }
                        self.server_send(ci, ServerMsg::Ack { results: acks });
                        self.pump_all();
                    }
                    Err(e) => {
                        self.log(format!("c{ci} push failed: {e}"));
                        self.server_crash();
                    }
                }
            }
            ClientMsg::Pull { from, .. } => {
                if let Some(c) = self.conns.get_mut(&ci) {
                    c.cursor = from;
                }
                self.pump(ci);
            }
            ClientMsg::Ping { nonce } => {
                let now = self.now;
                self.server_send(
                    ci,
                    ServerMsg::Pong {
                        nonce,
                        server_time: now,
                    },
                );
            }
            ClientMsg::DocSync {
                entry,
                slot,
                state_vector,
            } => {
                if let Ok(Some(u)) = self.engine().doc_diff(entry, &slot, &state_vector) {
                    self.server_send(
                        ci,
                        ServerMsg::DocDiff {
                            entry,
                            slot,
                            update: u,
                        },
                    );
                }
            }
        }
    }

    // ------------------------------------------------------------------ invariant checks

    /// Snapshot link resolutions before a push that only renames/moves (Invariant R).
    fn invariant_r_pre(&mut self, ops: &[jess_core::ops::Op]) -> Option<RPre> {
        let mut edited = HashSet::new();
        let mut has_rename = false;
        for op in ops {
            match &op.body {
                OpBody::Doc { entry, .. } => {
                    edited.insert(*entry);
                }
                OpBody::Meta(MetaOp::SetName { .. } | MetaOp::SetParent { .. }) => {
                    has_rename = true
                }
                OpBody::Meta(_) => return None,
            }
        }
        if !has_rename {
            return None;
        }
        let snap = self.resolutions(&edited);
        if self.verbose {
            let e = self.engine.as_ref().unwrap();
            let mut ps: Vec<String> = e
                .state
                .iter()
                .filter(|x| !x.purged)
                .filter_map(|x| {
                    e.state
                        .path_of(x.id)
                        .map(|p| format!("{p} {:?} live={}", x.id, x.is_live()))
                })
                .collect();
            ps.sort();
            self.log(format!("PRE paths {ps:?}"));
        }
        Some((edited, snap))
    }

    fn resolutions(&mut self, skip: &HashSet<Id>) -> HashMap<Id, Vec<Option<Id>>> {
        let e = self.engine.as_mut().unwrap();
        let docs: Vec<Id> = e
            .state
            .iter()
            .filter(|x| x.kind == KIND_MARKDOWN && !x.purged && !skip.contains(&x.id))
            .map(|x| x.id)
            .collect();
        let mut out = HashMap::new();
        for d in docs {
            let text = e.doc_text(d, SLOT_BODY).unwrap();
            let folder = e.state.folder_of(d);
            let v = extract(&text)
                .links
                .iter()
                .map(|l| e.ix.resolve(&l.target, l.syntax, &folder).map(|r| r.id))
                .collect();
            out.insert(d, v);
        }
        out
    }

    fn invariant_r_post(&mut self, (edited, pre): RPre) {
        let post = self.resolutions(&edited);
        let e = self.engine.as_ref().unwrap();
        for (doc, before) in pre {
            let Some(after) = post.get(&doc) else {
                continue;
            };
            assert_eq!(
                before.len(),
                after.len(),
                "seed {}: link count changed in {doc:?}",
                self.seed
            );
            for (i, (b, a)) in before.iter().zip(after).enumerate() {
                if let Some(target) = b {
                    if e.ix.contains(target) && a != b {
                        let e = self.engine.as_mut().unwrap();
                        let text = e.doc_text(doc, SLOT_BODY).unwrap();
                        let l = &extract(&text).links[i];
                        let paths: Vec<String> = e
                            .state
                            .iter()
                            .filter(|x| x.is_linkable())
                            .filter_map(|x| {
                                e.state.path_of(x.id).map(|p| format!("{p} {:?}", x.id))
                            })
                            .collect();
                        panic!("seed {}: Invariant R violated in doc {doc:?} (folder {:?}) link #{i} {l:?}: before {b:?} after {a:?}\ntext {text:?}\npaths {paths:#?}\n{}", self.seed, e.state.folder_of(doc), self.trace.join("\n"));
                    }
                }
            }
        }
    }

    fn check_resurrection(&mut self, purged_before: &HashSet<Id>) {
        let e = self.engine.as_ref().unwrap();
        for id in purged_before {
            let x = e.state.get(id).unwrap();
            if !x.purged {
                // Recovered into trash; a later op in the same push may rename it (LWW).
                assert!(
                    x.trashed.is_some() || x.name.starts_with(RECOVERED_PREFIX),
                    "seed {}: resurrection of {id:?} {x:?}",
                    self.seed
                );
            }
        }
    }

    // ------------------------------------------------------------------ workload

    fn live_ids(&self, ci: usize, pred: impl Fn(&jess_core::model::Entry) -> bool) -> Vec<Id> {
        let mut v: Vec<Id> = self.clients[ci]
            .c
            .view()
            .iter()
            .filter(|e| e.kind != "vault" && pred(e))
            .map(|e| e.id)
            .collect();
        v.sort();
        v
    }

    fn new_id(&mut self, ci: usize) -> Id {
        let mut r = [0u8; 10];
        for b in r.iter_mut() {
            *b = self.rng.next() as u8;
        }
        Id::new_v7(self.cnow(ci), r)
    }

    fn meta(&mut self, ci: usize, ops: Vec<MetaOp>) {
        let now = self.cnow(ci);
        let desc = format!("{ops:?}");
        match self.clients[ci].c.local_meta(ops, now) {
            Ok(o) => {
                self.log(format!("c{ci} meta {}", &desc[..desc.len().min(160)]));
                self.handle_output(ci, o, true);
            }
            Err(r) => self.log(format!("c{ci} meta refused locally: {r:?}")),
        }
    }

    fn random_parent(&mut self, ci: usize) -> Option<Id> {
        let folders = self.live_ids(ci, |e| e.is_folder() && e.is_live());
        if self.rng.chance(0.4) {
            None
        } else {
            self.rng.pick(&folders).copied()
        }
    }

    fn step_client(&mut self, ci: usize) {
        let roll = self.rng.below(100);
        let now = self.cnow(ci);
        match roll {
            0..=11 => {
                let id = self.new_id(ci);
                let parent = self.random_parent(ci);
                let folder = self.rng.chance(0.25);
                let name = if folder {
                    *self.rng.pick(FOLDERS).unwrap()
                } else {
                    *self.rng.pick(NAMES).unwrap()
                };
                self.meta(
                    ci,
                    vec![MetaOp::Create {
                        id,
                        kind: if folder { KIND_FOLDER } else { KIND_MARKDOWN }.into(),
                        parent,
                        name: name.into(),
                        tree_visible: true,
                        blob: None,
                        blob_info: None,
                        created_at: Some(now),
                        modified_at: None,
                        props: vec![],
                    }],
                );
            }
            12..=17 => {
                let ids = self.live_ids(ci, |e| !e.purged);
                if let Some(&id) = self.rng.pick(&ids) {
                    let is_folder = self.clients[ci].c.view().get(&id).unwrap().is_folder();
                    let name = if is_folder {
                        *self.rng.pick(FOLDERS).unwrap()
                    } else {
                        *self.rng.pick(NAMES).unwrap()
                    };
                    self.meta(
                        ci,
                        vec![MetaOp::SetName {
                            id,
                            name: name.into(),
                        }],
                    );
                }
            }
            18..=23 => {
                let ids = self.live_ids(ci, |e| e.is_live());
                if let Some(&id) = self.rng.pick(&ids) {
                    let parent = self.random_parent(ci);
                    if self.rng.chance(0.3) {
                        let is_folder = self.clients[ci].c.view().get(&id).unwrap().is_folder();
                        let name = if is_folder {
                            *self.rng.pick(FOLDERS).unwrap()
                        } else {
                            *self.rng.pick(NAMES).unwrap()
                        };
                        self.meta(
                            ci,
                            vec![
                                MetaOp::SetParent { id, parent },
                                MetaOp::SetName {
                                    id,
                                    name: name.into(),
                                },
                            ],
                        );
                    } else {
                        self.meta(ci, vec![MetaOp::SetParent { id, parent }]);
                    }
                }
            }
            24..=27 => {
                let ids = self.live_ids(ci, |e| e.is_live());
                if let Some(&id) = self.rng.pick(&ids) {
                    self.meta(ci, vec![MetaOp::Trash { id }]);
                }
            }
            28..=30 => {
                let ids = self.live_ids(ci, |e| e.trashed.is_some() && !e.purged);
                if let Some(&id) = self.rng.pick(&ids) {
                    let target = if self.rng.chance(0.5) {
                        self.clients[ci]
                            .c
                            .view()
                            .get(&id)
                            .unwrap()
                            .trashed
                            .unwrap()
                            .batch
                    } else {
                        id
                    };
                    self.meta(ci, vec![MetaOp::Restore { target }]);
                }
            }
            31..=32 => {
                let ids = self.live_ids(ci, |e| e.trashed.is_some() && !e.purged);
                if let Some(&id) = self.rng.pick(&ids) {
                    self.meta(ci, vec![MetaOp::Purge { id }]);
                }
            }
            33..=35 => {
                let ids = self.live_ids(ci, |e| !e.purged && !e.is_folder());
                if let Some(&id) = self.rng.pick(&ids) {
                    let v = self.rng.chance(0.5);
                    self.meta(ci, vec![MetaOp::SetVisible { id, visible: v }]);
                }
            }
            36..=40 => self.ingest(ci),
            41..=70 => self.edit_text(ci),
            71..=73 => {
                if self.rng.chance(0.5) {
                    self.disconnect(ci);
                } else {
                    self.connect(ci);
                }
            }
            74 => {
                self.log(format!("c{ci} CRASH"));
                self.crash_client(ci);
            }
            75..=84 => self.blob_step(ci, true),
            85..=87 => self.evict(ci),
            88..=89 => self.compact_client(ci),
            90..=91 => {
                let o = self.clients[ci].c.tick(now);
                self.handle_output(ci, o, false);
            }
            92..=93 => {
                // Request downloads of referenced blobs.
                let mut v: Vec<(Hash, u64)> = self.clients[ci]
                    .c
                    .view()
                    .iter()
                    .filter(|e| e.is_live())
                    .filter_map(|e| e.blob)
                    .map(|h| (h, 0))
                    .collect();
                v.sort();
                if let Some(&(h, _)) = self.rng.pick(&v) {
                    let size = self
                        .engine
                        .as_ref()
                        .unwrap()
                        .conn
                        .query_row(
                            "SELECT size FROM blobs WHERE hash = ?1",
                            [h.0.to_vec()],
                            |r| r.get::<_, i64>(0),
                        )
                        .unwrap_or(0) as u64;
                    if size > 0 {
                        let prio = self.rng.below(5) as u8;
                        let w = self.clients[ci].c.blobs.want(h, size, prio, now);
                        apply_writes(&mut self.clients[ci].kv, w);
                    }
                }
            }
            _ => {
                if let Some(t) = self.clients[ci].c.blobs.presence_task() {
                    self.do_blob_task(ci, t, false);
                }
            }
        }
    }

    fn ingest(&mut self, ci: usize) {
        let len = 1 + self.rng.below(80) as usize;
        let reuse = self.rng.chance(0.2);
        let bytes: Vec<u8> = if reuse {
            vec![7u8; 20]
        } else {
            (0..len).map(|_| self.rng.next() as u8).collect()
        };
        let h = Hash::of(&bytes);
        let now = self.cnow(ci);
        self.clients[ci].blob_bytes.insert(h, bytes.clone());
        self.clients[ci].created_blobs.insert(h);
        let mut w = self.clients[ci].c.blobs.ingest(h, bytes.len() as u64, now);
        let id = self.new_id(ci);
        let pdf = self.rng.chance(0.3);
        let parent = self.random_parent(ci);
        let existing = self.live_ids(ci, |e| e.is_live() && e.blob.is_some());
        let op = if !existing.is_empty() && self.rng.chance(0.25) {
            let id = *self.rng.pick(&existing).unwrap();
            MetaOp::SetBlob {
                id,
                blob: h,
                blob_info: Some(BlobInfo {
                    size: bytes.len() as u64,
                    ..Default::default()
                }),
            }
        } else {
            MetaOp::Create {
                id,
                kind: if pdf { KIND_PDF } else { KIND_MEDIA }.into(),
                parent,
                name: if pdf {
                    "doc.pdf".into()
                } else {
                    "Pasted image.png".into()
                },
                tree_visible: pdf,
                blob: Some(h),
                blob_info: Some(BlobInfo {
                    size: bytes.len() as u64,
                    mime: Some("image/png".into()),
                    width: Some(10),
                    height: Some(20),
                    orientation: None,
                }),
                created_at: Some(now),
                modified_at: None,
                props: vec![],
            }
        };
        match self.clients[ci].c.local_meta(vec![op], now) {
            Ok(mut o) => {
                w.extend(o.writes);
                o.writes = w;
                self.log(format!("c{ci} ingest {h:?}"));
                self.handle_output(ci, o, true);
            }
            Err(_) => apply_writes(&mut self.clients[ci].kv, w),
        }
    }

    fn edit_text(&mut self, ci: usize) {
        let docs = self.live_ids(ci, |e| e.kind == KIND_MARKDOWN && !e.purged);
        let Some(&id) = self.rng.pick(&docs) else {
            return;
        };
        let mut names: Vec<String> = self.clients[ci]
            .c
            .view()
            .iter()
            .filter(|e| e.is_linkable())
            .map(|e| e.name.clone())
            .collect();
        names.sort();
        let mut paths: Vec<String> = self.clients[ci]
            .c
            .view()
            .iter()
            .filter(|e| e.is_linkable())
            .filter_map(|e| self.clients[ci].c.view().path_of(e.id))
            .collect();
        paths.sort();
        let roll = self.rng.below(10);
        let snippet = match roll {
            0..=2 => {
                let n = self
                    .rng
                    .pick(&names)
                    .cloned()
                    .unwrap_or_else(|| "Missing".into());
                let n = n.strip_suffix(".md").unwrap_or(&n).to_string();
                match self.rng.below(4) {
                    0 => format!(" [[{n}]] "),
                    1 => format!(" [[{n}|alias]] "),
                    2 => format!(" ![[{n}#h]] "),
                    _ => format!(" [[{n}.md]] "),
                }
            }
            3 => {
                let p = self
                    .rng
                    .pick(&paths)
                    .cloned()
                    .unwrap_or_else(|| "x.md".into());
                if self.rng.chance(0.5) {
                    format!(" [t](<{p}>) ")
                } else {
                    format!(" [t]({}) ", p.replace(' ', "%20"))
                }
            }
            4 => {
                let p = self
                    .rng
                    .pick(&paths)
                    .cloned()
                    .unwrap_or_else(|| "x.md".into());
                format!(" [[{}]] ", p.strip_suffix(".md").unwrap_or(&p))
            }
            5 => "\r\n".into(),
            6 => "😀é".into(),
            _ => {
                ["hello ", "#tag ", "$x$ ", "`[[code]]` ", "\n"][self.rng.below(5) as usize].into()
            }
        };
        let del = self.rng.chance(0.25);
        let r1 = self.rng.next();
        let r2 = self.rng.next();
        let d = self.open_doc(ci, id);
        let len = ydoc::len16(d);
        let update = if del && len > 0 {
            let at = (r1 % len as u64) as u32;
            let n = 1 + (r2 % 6) as u32;
            // Don't split surrogate pairs: delete by whole chars via text.
            let text = ydoc::text(d);
            let u: Vec<u16> = text.encode_utf16().collect();
            let mut s = at as usize;
            while s > 0 && (0xDC00..0xE000).contains(&u[s]) {
                s -= 1;
            }
            let mut e = (s + n as usize).min(u.len());
            while e < u.len() && (0xDC00..0xE000).contains(&u[e]) {
                e += 1;
            }
            ydoc::remove(d, s as u32, (e - s) as u32)
        } else {
            let text = ydoc::text(d);
            let u: Vec<u16> = text.encode_utf16().collect();
            let mut at = if len == 0 {
                0
            } else {
                (r1 % (len as u64 + 1)) as usize
            };
            while at > 0 && at < u.len() && (0xDC00..0xE000).contains(&u[at]) {
                at -= 1;
            }
            ydoc::insert(d, at as u32, &snippet)
        };
        let now = self.cnow(ci);
        let o = self.clients[ci]
            .c
            .local_doc_update(id, SLOT_BODY, update, now);
        self.log(format!(
            "c{ci} edit {id:?} {}",
            if del {
                "del".to_string()
            } else {
                format!("{snippet:?}")
            }
        ));
        self.handle_output(ci, o, true);
    }

    fn blob_step(&mut self, ci: usize, faults: bool) {
        let tasks = self.clients[ci].c.blob_tasks();
        for t in tasks {
            self.do_blob_task(ci, t, faults);
        }
    }

    fn do_blob_task(&mut self, ci: usize, t: BlobTask, faults: bool) {
        let now = self.cnow(ci);
        if faults && self.rng.chance(self.faults.blob_fail) {
            // Transport failure; for chunk PUTs, maybe a torn partial write reached the server.
            if let BlobTask::PutChunk {
                hash,
                upload_id,
                offset,
                len,
                ..
            } = &t
            {
                if let Some(b) = self.clients[ci].blob_bytes.get(hash) {
                    let part = &b[*offset as usize..(*offset + *len / 2) as usize];
                    let _ = self.fs.write_chunk(upload_id, *offset, part);
                }
            }
            let w = self.clients[ci]
                .c
                .blob_result(BlobResult::Failed { task: t }, now);
            apply_writes(&mut self.clients[ci].kv, w);
            return;
        }
        let dup = faults && self.rng.chance(self.faults.dup);
        let r = self.serve_blob(ci, &t);
        if dup {
            let _ = self.serve_blob(ci, &t);
        }
        if let (
            BlobResult::RangeDone { ok: true, .. },
            BlobTask::GetRange {
                hash, offset, len, ..
            },
        ) = (&r, &t)
        {
            let data = self.fs.read_range(hash, *offset, *len).unwrap();
            let size = self.fs.size(hash).unwrap() as usize;
            let buf = self.clients[ci]
                .blob_bytes
                .entry(*hash)
                .or_insert_with(|| vec![0; size]);
            if buf.len() == size {
                buf[*offset as usize..*offset as usize + data.len()].copy_from_slice(&data);
            }
        }
        let w = self.clients[ci].c.blob_result(r, now);
        apply_writes(&mut self.clients[ci].kv, w);
    }

    fn serve_blob(&mut self, ci: usize, t: &BlobTask) -> BlobResult {
        let now = self.now;
        match t.clone() {
            BlobTask::Begin { hash, size } => {
                match self.engine().upload_begin(hash, size, None, now).unwrap() {
                    BeginUpload::Present => BlobResult::Began {
                        hash,
                        present: true,
                        upload_id: None,
                        received: Bitmap::default(),
                    },
                    BeginUpload::Session {
                        upload_id,
                        received,
                        ..
                    } => BlobResult::Began {
                        hash,
                        present: false,
                        upload_id: Some(upload_id),
                        received: Bitmap(received),
                    },
                    BeginUpload::TooLarge => panic!("too large"),
                }
            }
            BlobTask::PutChunk {
                hash,
                upload_id,
                index,
                offset,
                len,
            } => {
                let Some(row) = self.engine().upload_get(&upload_id).unwrap() else {
                    return BlobResult::UploadGone { hash };
                };
                let bytes = self.clients[ci].blob_bytes[&hash]
                    [offset as usize..(offset + len) as usize]
                    .to_vec();
                assert_eq!(row.hash, hash);
                self.fs.write_chunk(&upload_id, offset, &bytes).unwrap();
                self.engine()
                    .upload_chunk_done(&upload_id, index, now)
                    .unwrap();
                BlobResult::ChunkDone {
                    hash,
                    index,
                    ok: true,
                }
            }
            BlobTask::Complete { hash, upload_id } => {
                let Some(row) = self.engine().upload_get(&upload_id).unwrap() else {
                    let present = self.engine().blob_present(&hash).unwrap();
                    return if present {
                        BlobResult::Completed { hash, ok: true }
                    } else {
                        BlobResult::UploadGone { hash }
                    };
                };
                let ok = self.fs.finalize(&upload_id, &hash, row.size).unwrap();
                if ok {
                    self.engine()
                        .blob_stored(hash, row.size, Some(&upload_id), now)
                        .unwrap();
                    self.pump_all();
                } else {
                    self.engine().upload_discard(&upload_id).unwrap();
                }
                BlobResult::Completed { hash, ok }
            }
            BlobTask::GetRange { hash, index, .. } => {
                let ok = self.engine().blob_present(&hash).unwrap() && self.fs.exists(&hash);
                BlobResult::RangeDone { hash, index, ok }
            }
            BlobTask::Presence { hashes } => {
                let present = self.engine().presence(&hashes).unwrap();
                let missing = hashes
                    .into_iter()
                    .filter(|h| !present.contains(h))
                    .collect();
                BlobResult::Presence { present, missing }
            }
        }
    }

    fn evict(&mut self, ci: usize) {
        let mut hs: Vec<Hash> = self.clients[ci].blob_bytes.keys().copied().collect();
        hs.sort();
        let Some(&h) = self.rng.pick(&hs) else { return };
        let can = self.clients[ci].c.blobs.can_evict(&h);
        if let Some(w) = self.clients[ci].c.blobs.evict(h) {
            assert!(can);
            let present = self.engine().blob_present(&h).unwrap() && self.fs.exists(&h);
            let referenced = self
                .engine()
                .state
                .iter()
                .any(|e| !e.purged && e.blob == Some(h));
            assert!(
                present || !referenced,
                "seed {}: evicted {h:?} before the server had it",
                self.seed
            );
            apply_writes(&mut self.clients[ci].kv, w);
            self.clients[ci].blob_bytes.remove(&h);
            self.log(format!("c{ci} evict {h:?}"));
        }
    }

    fn compact_client(&mut self, ci: usize) {
        let mut docs: Vec<(Id, String)> = self.clients[ci].c.docs().cloned().collect();
        docs.sort();
        let Some((id, slot)) = self.rng.pick(&docs).cloned() else {
            return;
        };
        let seqs = self.clients[ci].c.doc_seqs(id, &slot);
        if seqs.len() < 2 {
            return;
        }
        let ups: Vec<Vec<u8>> = seqs
            .iter()
            .map(|s| self.clients[ci].kv[&kv::doc_key(id, &slot, *s)].clone())
            .collect();
        let merged = ydoc::merge(&ups).unwrap();
        let w = self.clients[ci]
            .c
            .compact_doc(id, &slot, merged, *seqs.last().unwrap());
        apply_writes(&mut self.clients[ci].kv, w);
    }

    fn step_server(&mut self) {
        let now = self.now;
        match self.rng.below(10) {
            0 => {
                self.engine().compact(now, 10).unwrap();
            }
            1 => {
                let rep = self.engine().gc(now, false).unwrap();
                let fs = self.fs.clone();
                self.engine().gc_sweep_files(&fs, &rep.deleted).unwrap();
                for u in &rep.expired_uploads {
                    self.fs.discard_tmp(u);
                }
                if !rep.deleted.is_empty() {
                    self.log(format!("server gc deleted {:?}", rep.deleted));
                }
                self.pump_all();
            }
            2 => {
                let purged_before: HashSet<Id> = self
                    .engine()
                    .state
                    .iter()
                    .filter(|e| e.purged)
                    .map(|e| e.id)
                    .collect();
                let n = self.engine().purge_expired_trash(now).unwrap();
                if n > 0 {
                    self.log(format!("server purged {n} expired"));
                }
                self.check_resurrection(&purged_before);
                self.pump_all();
            }
            3 if self.rng.chance(self.faults.server_crash * 3.0) => self.server_crash(),
            _ => {}
        }
    }

    /// Delivers due messages (with duplication and reordering).
    fn deliver(&mut self, faults: bool) {
        for ci in 0..self.clients.len() {
            // client → server
            loop {
                let due: Vec<usize> = (0..self.clients[ci].to_server.len())
                    .filter(|&i| self.clients[ci].to_server[i].at <= self.now)
                    .collect();
                if due.is_empty() || !self.clients[ci].connected {
                    break;
                }
                let idx = if faults && self.rng.chance(self.faults.reorder) {
                    due[self.rng.below(due.len() as u64) as usize]
                } else {
                    due[0]
                };
                let m = if faults && self.rng.chance(self.faults.dup) {
                    clone_client_msg(&self.clients[ci].to_server[idx].msg)
                } else {
                    self.clients[ci].to_server.remove(idx).msg
                };
                self.server_recv(ci, m);
                if faults && self.rng.chance(self.faults.disconnect) {
                    self.disconnect(ci);
                }
            }
            // server → client
            loop {
                let due: Vec<usize> = (0..self.clients[ci].to_client.len())
                    .filter(|&i| self.clients[ci].to_client[i].at <= self.now)
                    .collect();
                if due.is_empty() || !self.clients[ci].connected {
                    break;
                }
                let idx = if faults && self.rng.chance(self.faults.reorder) {
                    due[self.rng.below(due.len() as u64) as usize]
                } else {
                    due[0]
                };
                let m = if faults && self.rng.chance(self.faults.dup) {
                    self.clients[ci].to_client[idx].msg.clone()
                } else {
                    self.clients[ci].to_client.remove(idx).msg
                };
                let now = self.cnow(ci);
                if self.verbose {
                    let d = match &m {
                        ServerMsg::Ack { results } => format!("ack {results:?}"),
                        ServerMsg::Changes(c) => format!(
                            "changes {}..{} entries {:?} docs {:?}",
                            c.from,
                            c.to,
                            c.entries
                                .iter()
                                .map(|e| (e.id, e.seq, e.purged))
                                .collect::<Vec<_>>(),
                            c.docs
                                .iter()
                                .map(|d| (d.entry, d.seq, d.bytes.is_some()))
                                .collect::<Vec<_>>()
                        ),
                        other => format!("{other:?}").chars().take(80).collect(),
                    };
                    self.log(format!("c{ci} <- {d}"));
                }
                let o = self.clients[ci].c.on_message(m, now);
                self.handle_output(ci, o, faults);
            }
        }
    }

    fn run(&mut self, steps: usize) {
        for ci in 0..self.clients.len() {
            if self.rng.chance(0.8) {
                self.connect(ci);
            }
        }
        for _ in 0..steps {
            let big = self.rng.chance(0.02);
            self.now += 1 + self.rng.below(if big { 20 * 60_000 } else { 50 });
            let ci = self.rng.below(self.clients.len() as u64) as usize;
            self.step_client(ci);
            if self.rng.chance(0.1) {
                self.step_server();
            }
            self.deliver(true);
        }
        self.quiesce();
        self.check_final();
    }

    fn quiesce(&mut self) {
        self.log("---- quiesce".into());
        for ci in 0..self.clients.len() {
            self.connect(ci);
        }
        for round in 0..5000 {
            self.now += 40;
            self.deliver(false);
            for ci in 0..self.clients.len() {
                if !self.clients[ci].connected {
                    self.connect(ci);
                }
                if let Some(t) = self.clients[ci].c.blobs.presence_task() {
                    self.do_blob_task(ci, t, false);
                }
                self.blob_step(ci, false);
            }
            let head = self.engine().head;
            let idle = self
                .clients
                .iter()
                .all(|c| c.to_server.is_empty() && c.to_client.is_empty());
            if idle && self.clients.iter().all(|c| c.c.is_quiescent(head)) {
                return;
            }
            if idle && round % 50 == 49 {
                // Something is stuck waiting on a lost message: reconnect everyone.
                for ci in 0..self.clients.len() {
                    self.disconnect(ci);
                    self.connect(ci);
                }
            }
        }
        let st: Vec<String> = self
            .clients
            .iter()
            .map(|c| {
                format!(
                    "cursor {} pending {} unconfirmed {:?} status {:?}",
                    c.c.cursor,
                    c.c.pending_count(),
                    c.c.blobs.unconfirmed(),
                    c.c.status()
                )
            })
            .collect();
        let head = self.engine().head;
        panic!(
            "seed {}: did not quiesce (head {}): {:#?}\n{}",
            self.seed,
            head,
            st,
            self.trace.join("\n")
        );
    }

    fn check_final(&mut self) {
        let seed = self.seed;
        let e = self.engine.as_mut().unwrap();
        let problems = e.state.check_invariants();
        assert!(problems.is_empty(), "seed {seed}: invariants: {problems:?}");
        let mut server: Vec<_> = e.state.iter().cloned().collect();
        server.sort_by_key(|x| x.id);
        // Docs on the server.
        let mut texts = BTreeMap::new();
        for x in server
            .iter()
            .filter(|x| x.kind == KIND_MARKDOWN && !x.purged)
        {
            texts.insert(x.id, e.doc_text(x.id, SLOT_BODY).unwrap());
        }
        // The cached server docs equal their logs.
        for (id, t) in &texts {
            let rows = e.doc_rows(*id, SLOT_BODY).unwrap();
            let d = ydoc::from_updates(3, &rows).unwrap();
            if &ydoc::text(&d) != t {
                let dd = ydoc::new_doc(5);
                for r in &rows {
                    ydoc::apply(&dd, r).unwrap();
                    eprintln!(
                        "srow pending={} text={:?}",
                        ydoc::has_pending(&dd),
                        ydoc::text(&dd)
                    );
                }
                panic!(
                    "seed {seed}: server cache differs from log for {id:?}: {:?} vs {:?}\n{}",
                    ydoc::text(&d),
                    t,
                    self.trace.join("\n")
                );
            }
        }
        // Links index consistent with text.
        for (id, t) in &texts {
            let folder = e.state.folder_of(*id);
            let want: Vec<(String, Option<Id>)> = extract(t)
                .links
                .iter()
                .map(|l| {
                    (
                        l.target.clone(),
                        e.ix.resolve(&l.target, l.syntax, &folder).map(|r| r.id),
                    )
                })
                .collect();
            let mut st = e.conn.prepare("SELECT target, resolved FROM links WHERE src = ?1 AND slot = 'body' ORDER BY ord").unwrap();
            let got: Vec<(String, Option<Id>)> = st
                .query_map([id.0.to_vec()], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<Vec<u8>>>(1)?
                            .and_then(|b| Id::from_slice(&b)),
                    ))
                })
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            assert_eq!(
                got, want,
                "seed {seed}: links index out of date for {id:?}\n{t:?}"
            );
        }
        // No acknowledged doc update lost.
        for (id, slot, u) in &self.acked_docs {
            let x = e.state.get(id).unwrap();
            if x.purged {
                continue;
            }
            let rows = e.doc_rows(*id, slot).unwrap();
            let merged = ydoc::merge(&rows).unwrap();
            let sv = yrs::encode_state_vector_from_update_v1(&merged).unwrap();
            if ydoc::has_new_content(u, &merged).unwrap() {
                let us = yrs::updates::decoder::Decode::decode_v1(u)
                    .map(|x: yrs::Update| format!("{:?}", x.state_vector()))
                    .unwrap();
                let ss = format!(
                    "{:?}",
                    <yrs::StateVector as yrs::updates::decoder::Decode>::decode_v1(&sv).unwrap()
                );
                panic!("seed {seed}: acknowledged update lost for {id:?}: update {us} server {ss} text {:?}\n{}", texts.get(id), self.trace.join("\n"));
            }
        }
        // Blobs referenced by live entries are present and intact on the server.
        for x in server.iter().filter(|x| x.is_live()) {
            if let Some(h) = x.blob {
                assert!(
                    self.fs.exists(&h) && self.fs.verify(&h).unwrap(),
                    "seed {seed}: blob {h:?} of live {:?} missing on server",
                    x.id
                );
            }
        }
        for (ci, c) in self.clients.iter().enumerate() {
            let mut view: Vec<_> = c.c.view().iter().cloned().collect();
            view.sort_by_key(|x| x.id);
            if view.len() != server.len() {
                let vi: HashSet<Id> = view.iter().map(|x| x.id).collect();
                let missing: Vec<_> = server
                    .iter()
                    .filter(|x| !vi.contains(&x.id))
                    .map(|x| (x.id, x.name.clone(), x.seq, x.purged))
                    .collect();
                panic!(
                    "seed {seed}: client {ci} entry count {} vs {}: missing {missing:?} cursor {}",
                    view.len(),
                    server.len(),
                    c.c.cursor
                );
            }
            for (a, b) in view.iter().zip(&server) {
                assert_eq!(
                    a,
                    b,
                    "seed {seed}: client {ci} differs\n{}",
                    self.trace.join("\n")
                );
            }
            for (id, t) in &texts {
                let prefix = kv::doc_prefix(*id, SLOT_BODY);
                let ups: Vec<Vec<u8>> =
                    c.kv.range(prefix.clone()..)
                        .take_while(|(k, _)| k.starts_with(&prefix))
                        .map(|(_, v)| v.clone())
                        .collect();
                let d = ydoc::from_updates(1, &ups).unwrap();
                if &ydoc::text(&d) != t {
                    let srows: Vec<i64> = e
                        .conn
                        .prepare("SELECT seq FROM doc_updates WHERE entry_id = ?1 ORDER BY seq")
                        .unwrap()
                        .query_map([id.0.to_vec()], |r| r.get(0))
                        .unwrap()
                        .map(|r| r.unwrap())
                        .collect();
                    let dd = ydoc::new_doc(5);
                    for (k, v) in
                        c.kv.range(prefix.clone()..)
                            .take_while(|(k, _)| k.starts_with(&prefix))
                    {
                        let r = ydoc::apply(&dd, v);
                        eprintln!(
                            "row {:?} ok={} pending={} text={:?}",
                            kv::parse_doc_key(k).map(|x| x.2),
                            r.is_ok(),
                            ydoc::has_pending(&dd),
                            ydoc::text(&dd)
                        );
                    }
                    panic!("seed {seed}: client {ci} text of {id:?} differs: client rows {:?} server rows {:?} cursor {} head {}\n{:?}\n{:?}\n{}", c.c.doc_seqs(*id, SLOT_BODY), srows, c.c.cursor, e.head, ydoc::text(&d), t, self.trace.join("\n"));
                }
            }
            for x in server.iter().filter(|x| x.purged) {
                assert!(
                    c.c.doc_seqs(x.id, SLOT_BODY).is_empty(),
                    "seed {seed}: purged doc kept on client"
                );
            }
        }
    }
}

fn clone_client_msg(m: &ClientMsg) -> ClientMsg {
    m.clone()
}

fn apply_writes(kvs: &mut BTreeMap<Vec<u8>, Vec<u8>>, w: Vec<kv::Write>) {
    for x in w {
        match x {
            kv::Write::Put(k, v) => {
                kvs.insert(k, v);
            }
            kv::Write::Del(k) => {
                kvs.remove(&k);
            }
        }
    }
}

fn run_seed(seed: u64, verbose: bool) {
    let mut sim = Sim::new(seed);
    sim.verbose = verbose;
    let steps = 100 + (seed % 7) as usize * 40;
    sim.run(steps);
}

#[test]
fn simulation() {
    if let Ok(s) = std::env::var("SIM_SEED") {
        run_seed(s.parse().unwrap(), true);
        return;
    }
    let n: u64 = std::env::var("SIM_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let start: u64 = std::env::var("SIM_START")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let t = std::time::Instant::now();
    for seed in start..start + n {
        let r = std::panic::catch_unwind(|| run_seed(seed, false));
        if r.is_err() {
            panic!("simulation failed at seed {seed}; rerun with SIM_SEED={seed}");
        }
    }
    eprintln!("simulation: {n} seeds in {:?}", t.elapsed());
}
