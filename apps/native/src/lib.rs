//! The native client backend (DESIGN §2, §11.2): what the web's sync worker does, in Rust, for
//! the Tauri apps. jess-core's sans-IO sync client, `app.db` (rusqlite) as the source of truth,
//! blobs as files, `index.db` for links/tags/search, and the transport (WebSocket with HTTP
//! fallback, chunked blob transfers). No Tauri dependency: the app shell is a thin layer of
//! commands over [`Native`], and everything here is testable headlessly.
//!
//! Every core call and the commit of its writes happen under one lock, in order, before any
//! frame is sent: the same discipline as the worker's `run` queue.

pub mod blobfiles;
pub mod index;
pub mod io;
pub mod store;
mod transport;

use blobfiles::BlobFiles;
use index::Index;
use jess_core::client::{Client, Event, Output};
use jess_core::kv::{self, Write};
use jess_core::links::{extract, Syntax};
use jess_core::resolve::ResolveIndex;
use jess_core::{Hash, Id};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use store::Store;
use tokio::sync::{mpsc, Notify};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const CHUNK: u64 = jess_core::blobs::CHUNK_SIZE;
/// LRU cap for "download on demand" (§7.5): 2 GB on devices.
pub const ON_DEMAND_CAP: u64 = 2 << 30;

/// Events for the UI, in the same shapes the web worker posts (`{ev: …}`).
pub type EventSink = Arc<dyn Fn(Value) + Send + Sync>;

/// Bytes and their media type, if known.
pub type Bytes = (Vec<u8>, Option<String>);

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `bytes=a-b`, `bytes=a-`, `bytes=-n` (single range) → inclusive `(a, b)`.
pub fn parse_range(h: &str, size: u64) -> Option<(u64, u64)> {
    let r = h.trim().strip_prefix("bytes=")?;
    if r.contains(',') || size == 0 {
        return None;
    }
    let (a, b) = r.split_once('-')?;
    let (a, b) = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        (size.saturating_sub(n), size - 1)
    } else {
        let a: u64 = a.parse().ok()?;
        let b = if b.is_empty() {
            size - 1
        } else {
            b.parse::<u64>().ok()?.min(size - 1)
        };
        (a, b)
    };
    (a <= b && a < size).then_some((a, b))
}

pub(crate) struct State {
    pub client: Client,
    ix: ResolveIndex,
    store: Store,
    open_docs: HashMap<Id, usize>,
    /// Local Yjs updates waiting to be coalesced (30 ms).
    pending_doc: HashMap<Id, Vec<Vec<u8>>>,
    /// Outgoing frames of the live WebSocket, if any.
    pub ws: Option<mpsc::UnboundedSender<Vec<u8>>>,
    /// Frames for the next HTTP long-poll request (fallback transport).
    pub http_outbox: Vec<Vec<u8>>,
    pub http_mode: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Conf {
    pub server: Option<String>,
    pub token: Option<String>,
    pub foreground: bool,
    pub everything: bool,
    pub fatal: Option<String>,
}

pub(crate) struct Inner {
    pub dir: PathBuf,
    pub st: Mutex<State>,
    pub blobs: BlobFiles,
    index: Mutex<Option<Index>>,
    dirty: Mutex<BTreeSet<Id>>,
    pub events: EventSink,
    pub conf: Mutex<Conf>,
    /// Wakes the transport (new token/server, online, foreground).
    pub wake_transport: Notify,
    /// Wakes the blob pump.
    pub wake_pump: Notify,
    index_timer: Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub import: Mutex<Option<io::ImportState>>,
    pub import_cancel: std::sync::atomic::AtomicBool,
    /// The runtime `open` ran in. Calls may come from any thread (the GUI's main thread in the
    /// apps), so background work is always spawned through this handle.
    pub rt: tokio::runtime::Handle,
}

/// Handle on the native backend. Cheap to clone.
#[derive(Clone)]
pub struct Native {
    pub(crate) inner: Arc<Inner>,
}

impl Native {
    /// Opens (or creates) the local stores under `dir`. Must be called inside a tokio runtime;
    /// starts the transport, blob pump and index tasks.
    pub fn open(dir: &Path, events: EventSink) -> Result<Native, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let mut store = Store::open(&dir.join("app.db")).map_err(|e| e.to_string())?;
        let items = store.load().map_err(|e| e.to_string())?;
        let (client, init) = Client::load(items, rand::random::<u64>() >> 1 | 1, CHUNK);
        store.commit(&init).map_err(|e| e.to_string())?;
        let ix = ResolveIndex::build(client.view());
        let meta = |k: &str| {
            store
                .meta_get(k)
                .ok()
                .flatten()
                .and_then(|v| String::from_utf8(v).ok())
        };
        let conf = Conf {
            server: meta("server"),
            token: meta("token"),
            foreground: true,
            everything: meta("offline").as_deref() != Some("on-demand"),
            fatal: None,
        };
        let inner = Arc::new(Inner {
            dir: dir.to_path_buf(),
            st: Mutex::new(State {
                client,
                ix,
                store,
                open_docs: HashMap::new(),
                pending_doc: HashMap::new(),
                ws: None,
                http_outbox: Vec::new(),
                http_mode: false,
            }),
            blobs: BlobFiles::new(dir.join("blobs")).map_err(|e| e.to_string())?,
            index: Mutex::new(None),
            dirty: Mutex::new(BTreeSet::new()),
            events,
            conf: Mutex::new(conf),
            wake_transport: Notify::new(),
            wake_pump: Notify::new(),
            index_timer: Mutex::new(None),
            import: Mutex::new(None),
            import_cancel: Default::default(),
            rt: tokio::runtime::Handle::current(),
        });
        let n = Native { inner };
        transport::spawn(n.clone());
        let n2 = n.clone();
        n.inner.rt.spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            n2.ensure_index().await;
            n2.schedule_policy();
        });
        Ok(n)
    }

    pub(crate) fn emit(&self, v: Value) {
        (self.inner.events)(v)
    }

    // ------------------------------------------------------------------ apply

    /// Commits a core output and dispatches its frames and events. Called with the state lock
    /// held (`st`), so commits and sends happen in core order.
    pub(crate) fn apply_locked(&self, st: &mut State, o: Output) {
        if let Err(e) = st.store.commit(&o.writes) {
            // The store is the source of truth: if it can't be written, nothing may be sent.
            tracing::error!("app.db commit failed: {e}");
            self.emit(json!({"ev": "fatal", "message": format!("local database error: {e}")}));
            return;
        }
        for m in &o.send {
            let bytes = jess_core::proto::encode(m);
            if let Some(ws) = &st.ws {
                let _ = ws.send(bytes);
            } else if st.http_mode {
                st.http_outbox.push(bytes);
            }
        }
        let mut changed: Vec<Id> = Vec::new();
        let mut status = false;
        for e in o.events {
            match e {
                Event::EntriesChanged(ids) => changed.extend(ids),
                Event::DocRemote {
                    entry,
                    slot,
                    update,
                } => {
                    if slot == "body" {
                        if st.open_docs.contains_key(&entry) {
                            self.emit(
                                json!({"ev": "doc", "id": entry.to_string(), "update": update}),
                            );
                        }
                        self.mark_dirty(entry);
                    }
                }
                Event::Rejected { op_id, reason } => self.emit(
                    json!({"ev": "rejected", "opId": op_id, "reason": format!("{reason:?}")}),
                ),
                Event::Purged(id) => {
                    if let Some(ix) = self.inner.index.lock().expect("lock").as_mut() {
                        let _ = ix.remove(&id.to_string());
                    }
                }
                Event::ConnectionDead => {
                    st.ws = None; // dropping the sender ends the socket task
                }
                Event::Fatal(m) => {
                    self.inner.conf.lock().expect("lock").fatal = Some(m.clone());
                    self.emit(json!({"ev": "fatal", "message": m}));
                }
                Event::StatusChanged => status = true,
            }
        }
        if !changed.is_empty() {
            changed.sort();
            changed.dedup();
            st.ix.sync(st.client.view(), &changed);
            let list = jess_core::json::entries_view(&st.client, &changed);
            let mut blobs = false;
            for (id, e) in changed.iter().zip(&list) {
                if e["kind"] == "markdown" {
                    self.mark_dirty(*id);
                }
                blobs |= e["blob"].is_string();
            }
            self.emit(json!({"ev": "entries", "list": list}));
            if blobs {
                // Not here: the state lock is held.
                let n = self.clone();
                self.inner.rt.spawn(async move { n.schedule_policy() });
            }
        }
        if status {
            self.emit(json!({"ev": "status", "status": self.status_locked(st)}));
        }
        self.inner.wake_pump.notify_one();
    }

    pub(crate) fn commit(&self, w: Vec<Write>) {
        let mut st = self.inner.st.lock().expect("lock");
        self.apply_locked(
            &mut st,
            Output {
                writes: w,
                ..Default::default()
            },
        );
    }

    fn status_locked(&self, st: &State) -> Value {
        let mut s = jess_core::json::status(&st.client);
        let c = self.inner.conf.lock().expect("lock");
        if let Some(f) = &c.fatal {
            s["state"] = json!("error");
            s["error"] = json!(f);
        } else if c.token.is_none() {
            s["state"] = json!("offline");
        }
        s
    }

    // ------------------------------------------------------------------ session

    /// The UI's init: entries, status, replica, vault.
    pub fn init(&self) -> Value {
        let st = self.inner.st.lock().expect("lock");
        json!({
            "entries": jess_core::json::view(&st.client),
            "status": self.status_locked(&st),
            "replica": format!("{:x}", st.client.replica),
            "vaultId": st.client.vault.map(|v| v.to_string()),
        })
    }

    pub fn server(&self) -> Option<String> {
        self.inner.conf.lock().expect("lock").server.clone()
    }

    pub fn token(&self) -> Option<String> {
        self.inner.conf.lock().expect("lock").token.clone()
    }

    /// Sets the server base URL (e.g. `https://notes.example.com`).
    pub fn set_server(&self, url: Option<String>) {
        let url = url
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty());
        {
            let st = self.inner.st.lock().expect("lock");
            let _ = st
                .store
                .meta_put("server", url.as_deref().map(str::as_bytes));
        }
        self.inner.conf.lock().expect("lock").server = url;
        self.inner.wake_transport.notify_one();
    }

    pub fn set_token(&self, token: Option<String>) {
        {
            let st = self.inner.st.lock().expect("lock");
            let _ = st
                .store
                .meta_put("token", token.as_deref().map(str::as_bytes));
        }
        {
            let mut c = self.inner.conf.lock().expect("lock");
            c.token = token;
            c.fatal = None;
        }
        self.inner.wake_transport.notify_one();
    }

    pub fn meta_get(&self, k: &str) -> Option<String> {
        let st = self.inner.st.lock().expect("lock");
        st.store
            .meta_get(k)
            .ok()
            .flatten()
            .and_then(|v| String::from_utf8(v).ok())
    }

    pub fn meta_put(&self, k: &str, v: Option<&str>) {
        let st = self.inner.st.lock().expect("lock");
        let _ = st.store.meta_put(k, v.map(str::as_bytes));
    }

    pub fn status(&self) -> Value {
        let st = self.inner.st.lock().expect("lock");
        self.status_locked(&st)
    }

    pub fn quarantine(&self) -> Value {
        let st = self.inner.st.lock().expect("lock");
        Value::Array(jess_core::json::quarantine(&st.client))
    }

    pub fn set_foreground(&self, f: bool) {
        self.inner.conf.lock().expect("lock").foreground = f;
        if f {
            self.inner.wake_transport.notify_one();
        }
    }

    /// Network came back (or the app resumed): probe now.
    pub fn online(&self) {
        self.inner.wake_transport.notify_one();
    }

    // ------------------------------------------------------------------ meta

    /// Applies meta intents (the UI's JSON ops) as one atomic group.
    pub fn intent(&self, ops_json: &str) -> Result<(), String> {
        let ops = jess_core::json::parse_ops(ops_json)?;
        let mut st = self.inner.st.lock().expect("lock");
        match st.client.local_meta(ops, now_ms()) {
            Ok(o) => {
                self.apply_locked(&mut st, o);
                Ok(())
            }
            Err(r) => Err(format!("{r:?}")),
        }
    }

    pub fn resolve(&self, target: &str, markdown: bool, src: Option<&str>) -> Option<String> {
        let st = self.inner.st.lock().expect("lock");
        let folder = src
            .and_then(Id::parse)
            .map(|s| st.client.view().folder_of(s))
            .unwrap_or_default();
        let syn = if markdown {
            Syntax::Markdown
        } else {
            Syntax::Wiki
        };
        st.client
            .redirects()
            .resolve(&st.ix, target, syn, &folder)
            .map(|r| r.id.to_string())
    }

    // ------------------------------------------------------------------ docs

    fn doc_rows_locked(st: &State, id: Id) -> Vec<Vec<u8>> {
        let mut rows = st
            .store
            .scan_prefix(&kv::doc_prefix(id, "body"))
            .unwrap_or_default();
        rows.extend(st.client.pending_doc_updates(id, "body"));
        rows
    }

    fn flush_doc_locked(&self, st: &mut State, id: Id) {
        let Some(ups) = st.pending_doc.remove(&id) else {
            return;
        };
        let merged = if ups.len() == 1 {
            ups.into_iter().next().expect("one")
        } else {
            match jess_core::doc::merge(&ups) {
                Ok(m) => m,
                Err(_) => return,
            }
        };
        let o = st.client.local_doc_update(id, "body", merged, now_ms());
        self.apply_locked(st, o);
        self.mark_dirty(id);
    }

    /// Opens a note's text: every stored update (the host builds its Y.Doc from them).
    pub fn open_doc(&self, id: &str) -> Result<Vec<Vec<u8>>, String> {
        let id = Id::parse(id).ok_or("bad id")?;
        let mut st = self.inner.st.lock().expect("lock");
        *st.open_docs.entry(id).or_default() += 1;
        self.flush_doc_locked(&mut st, id);
        Ok(Self::doc_rows_locked(&st, id))
    }

    pub fn close_doc(&self, id: &str) {
        let Some(id) = Id::parse(id) else { return };
        let mut st = self.inner.st.lock().expect("lock");
        self.flush_doc_locked(&mut st, id);
        if let Some(n) = st.open_docs.get_mut(&id) {
            *n -= 1;
            if *n == 0 {
                st.open_docs.remove(&id);
            }
        }
    }

    /// A local Yjs update from the editor; coalesced for 30 ms.
    pub fn doc_update(&self, id: &str, update: Vec<u8>) {
        let Some(id) = Id::parse(id) else { return };
        let first = {
            let mut st = self.inner.st.lock().expect("lock");
            let v = st.pending_doc.entry(id).or_default();
            v.push(update);
            v.len() == 1
        };
        if first {
            let n = self.clone();
            self.inner.rt.spawn(async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                let mut st = n.inner.st.lock().expect("lock");
                n.flush_doc_locked(&mut st, id);
            });
        }
    }

    /// Commits every coalesced edit now.
    pub fn flush(&self) {
        let mut st = self.inner.st.lock().expect("lock");
        let ids: Vec<Id> = st.pending_doc.keys().copied().collect();
        for id in ids {
            self.flush_doc_locked(&mut st, id);
        }
    }

    pub fn doc_text(&self, id: &str) -> Result<String, String> {
        let id = Id::parse(id).ok_or("bad id")?;
        Ok(self.doc_text_of(id))
    }

    pub(crate) fn doc_text_of(&self, id: Id) -> String {
        let rows = {
            let mut st = self.inner.st.lock().expect("lock");
            self.flush_doc_locked(&mut st, id);
            Self::doc_rows_locked(&st, id)
        };
        let d = jess_core::doc::new_doc(1);
        for r in &rows {
            let _ = jess_core::doc::apply(&d, r);
        }
        jess_core::doc::text(&d)
    }

    /// All confirmed + pending updates of a note (for writing a new version of its text).
    pub(crate) fn doc_updates_of(&self, id: Id) -> Vec<Vec<u8>> {
        let mut st = self.inner.st.lock().expect("lock");
        self.flush_doc_locked(&mut st, id);
        Self::doc_rows_locked(&st, id)
    }

    // ------------------------------------------------------------------ index

    fn mark_dirty(&self, id: Id) {
        self.inner.dirty.lock().expect("lock").insert(id);
        if self.inner.index.lock().expect("lock").is_none() {
            return;
        }
        let n = self.clone();
        let mut t = self.inner.index_timer.lock().expect("lock");
        if let Some(h) = t.take() {
            h.abort();
        }
        {
            *t = Some(self.inner.rt.spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                n.reindex_dirty().await;
            }));
        }
    }

    pub(crate) async fn ensure_index(&self) {
        if self.inner.index.lock().expect("lock").is_some() {
            return;
        }
        let path = self.inner.dir.join("index.db");
        let ix = match tokio::task::spawn_blocking(move || Index::open(&path)).await {
            Ok(Ok(ix)) => ix,
            Ok(Err(e)) => {
                tracing::warn!("index.db: {e}");
                return;
            }
            Err(_) => return,
        };
        let live: Vec<(Id, String)> = {
            let st = self.inner.st.lock().expect("lock");
            st.client
                .view()
                .iter()
                .filter(|e| e.kind == "markdown" && !e.purged && e.blob.is_none())
                .map(|e| (e.id, e.id.to_string()))
                .collect()
        };
        {
            let mut g = self.inner.index.lock().expect("lock");
            if g.is_some() {
                return;
            }
            let mut ix = ix;
            let known = ix.indexed_hashes().unwrap_or_default();
            let ids: BTreeSet<&String> = live.iter().map(|x| &x.1).collect();
            for k in known.keys() {
                if !ids.contains(k) {
                    let _ = ix.remove(k);
                }
            }
            *g = Some(ix);
        }
        self.inner
            .dirty
            .lock()
            .expect("lock")
            .extend(live.iter().map(|x| x.0));
        self.reindex_dirty().await;
        self.schedule_pdf_index(Duration::ZERO);
    }

    pub(crate) async fn reindex_dirty(&self) {
        let ids: Vec<Id> = std::mem::take(&mut *self.inner.dirty.lock().expect("lock"))
            .into_iter()
            .collect();
        if ids.is_empty() || self.inner.index.lock().expect("lock").is_none() {
            return;
        }
        for id in ids {
            let e = self
                .inner
                .st
                .lock()
                .expect("lock")
                .client
                .view()
                .get(&id)
                .cloned();
            let text = match &e {
                Some(e) if e.kind == "markdown" && !e.purged && e.blob.is_none() => {
                    Some(self.doc_text_of(id))
                }
                _ => None,
            };
            let mut g = self.inner.index.lock().expect("lock");
            let Some(ix) = g.as_mut() else { return };
            match (e, text) {
                (Some(e), Some(text)) => {
                    let ex = extract(&text);
                    let name = e.name.strip_suffix(".md").unwrap_or(&e.name).to_string();
                    let _ = ix.upsert(&id.to_string(), &name, &text, &ex);
                }
                _ => {
                    let _ = ix.remove(&id.to_string());
                }
            }
        }
        self.emit(json!({"ev": "indexed"}));
    }

    fn name_keys(name: &str) -> Vec<String> {
        vec![index::target_key(name), jess_core::names::lookup_key(name)]
    }

    pub async fn backlinks(&self, id: &str) -> Vec<Value> {
        self.ensure_index().await;
        self.reindex_dirty().await;
        let Some(target) = Id::parse(id) else {
            return vec![];
        };
        let name = match self
            .inner
            .st
            .lock()
            .expect("lock")
            .client
            .view()
            .get(&target)
        {
            Some(e) => e.name.clone(),
            None => return vec![],
        };
        let links = {
            let g = self.inner.index.lock().expect("lock");
            match g.as_ref() {
                Some(ix) => ix
                    .links_by_keys(&Self::name_keys(&name))
                    .unwrap_or_default(),
                None => return vec![],
            }
        };
        let mut out: Vec<(String, usize, bool)> = Vec::new();
        for l in links {
            if l.src == id
                || self.resolve(&l.target, l.markdown, Some(&l.src)).as_deref() != Some(id)
            {
                continue;
            }
            match out.iter_mut().find(|o| o.0 == l.src) {
                Some(o) => {
                    o.1 += 1;
                    o.2 |= l.embed;
                }
                None => out.push((l.src, 1, l.embed)),
            }
        }
        out.into_iter()
            .map(|(src, count, embed)| json!({"src": src, "count": count, "embed": embed}))
            .collect()
    }

    pub async fn tags(&self) -> Vec<Value> {
        self.ensure_index().await;
        self.reindex_dirty().await;
        self.inner
            .index
            .lock()
            .expect("lock")
            .as_ref()
            .and_then(|ix| ix.tags().ok())
            .unwrap_or_default()
    }

    pub async fn notes_with_tag(&self, tag: &str) -> Vec<String> {
        self.ensure_index().await;
        self.reindex_dirty().await;
        self.inner
            .index
            .lock()
            .expect("lock")
            .as_ref()
            .and_then(|ix| ix.notes_with_tag(tag).ok())
            .unwrap_or_default()
    }

    pub async fn search(&self, q: &str) -> Vec<Value> {
        self.ensure_index().await;
        self.reindex_dirty().await;
        self.inner
            .index
            .lock()
            .expect("lock")
            .as_ref()
            .map(|ix| ix.search(q, 50))
            .unwrap_or_default()
    }

    /// Links (from live or trashed notes) resolving to each attachment (§7.8).
    pub async fn attachment_refs(&self) -> Value {
        self.ensure_index().await;
        self.reindex_dirty().await;
        let atts: Vec<(Id, String)> = {
            let st = self.inner.st.lock().expect("lock");
            st.client
                .view()
                .iter()
                .filter(|e| (e.kind == "media" || e.kind == "pdf") && !e.purged)
                .map(|e| (e.id, e.name.clone()))
                .collect()
        };
        let mut refs: HashMap<String, u64> = atts.iter().map(|a| (a.0.to_string(), 0)).collect();
        let keys: Vec<String> = atts
            .iter()
            .flat_map(|a| Self::name_keys(&a.1))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let links = self
            .inner
            .index
            .lock()
            .expect("lock")
            .as_ref()
            .and_then(|ix| ix.links_by_keys(&keys).ok())
            .unwrap_or_default();
        let mut cache: HashMap<(String, bool, String), Option<String>> = HashMap::new();
        for l in links {
            let k = (l.src.clone(), l.markdown, l.target.clone());
            let r = cache
                .entry(k)
                .or_insert_with(|| self.resolve(&l.target, l.markdown, Some(&l.src)))
                .clone();
            if let Some(r) = r {
                if let Some(n) = refs.get_mut(&r) {
                    *n += 1;
                }
            }
        }
        json!(refs)
    }

    // ------------------------------------------------------------------ PDF text (§8)

    pub(crate) fn schedule_pdf_index(&self, delay: Duration) {
        let n = self.clone();
        {
            self.inner.rt.spawn(async move {
                tokio::time::sleep(delay).await;
                n.index_pdfs().await;
            });
        }
    }

    async fn index_pdfs(&self) {
        let live: Vec<(String, String, String)> = {
            let st = self.inner.st.lock().expect("lock");
            st.client
                .view()
                .iter()
                .filter(|e| e.kind == "pdf" && !e.purged && e.trashed.is_none())
                .filter_map(|e| Some((e.id.to_string(), e.name.clone(), e.blob?.to_hex())))
                .collect()
        };
        let known = match self.inner.index.lock().expect("lock").as_ref() {
            Some(ix) => ix.indexed_pdfs().unwrap_or_default(),
            None => return,
        };
        let mut retry = false;
        for (id, name, blob) in live {
            if known.get(&id) == Some(&blob) {
                continue;
            }
            let path = format!("/api/blobs/{blob}/derived/pdf-text");
            let r = {
                let n = self.clone();
                tokio::task::spawn_blocking(move || n.http_get(&path, None)).await
            };
            match r {
                Ok(Ok((200, body))) => {
                    let pages: Vec<String> = serde_json::from_slice::<Value>(&body)
                        .ok()
                        .and_then(|v| v["pages"].as_array().cloned())
                        .unwrap_or_default()
                        .into_iter()
                        .map(|p| p.as_str().unwrap_or("").to_string())
                        .collect();
                    if let Some(ix) = self.inner.index.lock().expect("lock").as_mut() {
                        let _ = ix.upsert_pdf(&id, &name, &blob, &pages);
                    }
                }
                _ => retry = true,
            }
        }
        if retry {
            self.schedule_pdf_index(Duration::from_secs(60));
        }
    }

    // ------------------------------------------------------------------ blobs

    /// Streams a file into local blob storage and returns `{hash, size, info}`; with `name`,
    /// the facts (mime, dimensions) are read from its header.
    pub fn ingest_reader(&self, name: &str, r: &mut dyn std::io::Read) -> Result<Value, String> {
        let (h, size, header) = self.inner.blobs.ingest(r).map_err(|e| e.to_string())?;
        let w = self
            .inner
            .st
            .lock()
            .expect("lock")
            .client
            .blobs
            .ingest(h, size, now_ms());
        self.commit(w);
        let info = jess_core::import::blob_info_from_header(name, &header, size);
        Ok(json!({"hash": h.to_hex(), "size": size, "info": jess_core::json::blob_info(&info)}))
    }

    pub fn ingest_bytes(&self, name: &str, bytes: &[u8]) -> Result<Value, String> {
        self.ingest_reader(name, &mut &bytes[..])
    }

    pub fn ingest_path(&self, name: &str, path: &Path) -> Result<Value, String> {
        let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
        self.ingest_reader(name, &mut f)
    }

    pub fn blob_want(&self, hash: &str, size: u64, prio: u8) {
        let Some(h) = Hash::parse_hex(hash) else {
            return;
        };
        let w = self
            .inner
            .st
            .lock()
            .expect("lock")
            .client
            .blobs
            .want(h, size, prio, now_ms());
        self.commit(w);
    }

    pub fn blob_is_local(&self, h: &Hash) -> bool {
        self.inner.st.lock().expect("lock").client.blobs.is_local(h)
    }

    /// Whether chunks covering `[begin, end)` are all on this device.
    pub(crate) fn range_is_local(&self, h: &Hash, begin: u64, end: u64) -> bool {
        let st = self.inner.st.lock().expect("lock");
        let Some(b) = st.client.blobs.local.get(h) else {
            return false;
        };
        if end == 0 || end > b.size {
            return false;
        }
        (begin / CHUNK..=(end - 1) / CHUNK).all(|i| b.have.get(i as u32))
    }

    pub(crate) fn blob_size(&self, h: &Hash) -> Option<u64> {
        let st = self.inner.st.lock().expect("lock");
        st.client
            .blobs
            .local
            .get(h)
            .map(|b| b.size)
            .or_else(|| st.client.blob_facts(h).map(|f| f.size))
    }

    /// Bytes `[begin, end)` of a blob: local chunks first, else an authenticated range fetch.
    pub fn blob_range(&self, hash: &str, begin: u64, end: u64) -> Result<Vec<u8>, String> {
        let h = Hash::parse_hex(hash).ok_or("bad hash")?;
        if end <= begin {
            return Ok(vec![]);
        }
        if self.range_is_local(&h, begin, end) {
            return self
                .inner
                .blobs
                .read_at(&h, begin, end - begin)
                .map_err(|e| e.to_string());
        }
        let (code, body) = self.http_get(
            &format!("/api/blobs/{hash}"),
            Some(&format!("bytes={begin}-{}", end - 1)),
        )?;
        if code == 200 || code == 206 {
            Ok(body)
        } else {
            Err(format!("blob range {code}"))
        }
    }

    /// A blob (`orig`) or derived variant; derived files are cached on disk (small, kept).
    pub fn blob_read(&self, hash: &str, variant: &str) -> Result<Option<Bytes>, String> {
        let h = Hash::parse_hex(hash).ok_or("bad hash")?;
        if variant != "orig" {
            let cache = self.inner.dir.join("derived").join(variant).join(hash);
            if let Ok(b) = std::fs::read(&cache) {
                let mime = std::fs::read_to_string(cache.with_extension("type")).ok();
                return Ok(Some((b, mime)));
            }
            if let Ok((200, body, mime)) =
                self.http_get_typed(&format!("/api/blobs/{hash}/derived/{variant}"))
            {
                let _ = std::fs::create_dir_all(cache.parent().expect("parent"));
                let _ = std::fs::write(&cache, &body);
                if let Some(m) = &mime {
                    let _ = std::fs::write(cache.with_extension("type"), m);
                }
                return Ok(Some((body, mime)));
            }
            if variant == "pdf-thumb" || variant == "pdf-text" {
                return Ok(None);
            }
        }
        let mime = self
            .inner
            .st
            .lock()
            .expect("lock")
            .client
            .blob_facts(&h)
            .and_then(|f| f.mime.clone());
        if self.blob_is_local(&h) {
            let size = self.blob_size(&h).unwrap_or(0);
            return Ok(Some((
                self.inner
                    .blobs
                    .read_at(&h, 0, size)
                    .map_err(|e| e.to_string())?,
                mime,
            )));
        }
        match self.http_get(&format!("/api/blobs/{hash}"), None) {
            Ok((200, b)) => Ok(Some((b, mime))),
            _ => Ok(None),
        }
    }

    /// The `jess-blob://localhost/{hash}/{variant}` handler (§7.7): local bytes when present,
    /// else a P0 download is queued and the request is streamed from the server. Supports
    /// `Range` for the original. Returns `(status, headers, body)`.
    pub fn serve_blob(
        &self,
        path: &str,
        range: Option<&str>,
    ) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let mut parts = path.trim_start_matches('/').split('/');
        let (Some(hash), variant) = (parts.next(), parts.next().unwrap_or("orig")) else {
            return (400, vec![], b"bad path".to_vec());
        };
        let Some(h) = Hash::parse_hex(hash) else {
            return (400, vec![], b"bad hash".to_vec());
        };
        let facts = self
            .inner
            .st
            .lock()
            .expect("lock")
            .client
            .blob_facts(&h)
            .cloned();
        let mime = facts
            .as_ref()
            .and_then(|f| f.mime.clone())
            .unwrap_or_else(|| "application/octet-stream".into());
        let mut headers = vec![("cache-control".to_string(), "no-store".to_string())];
        if mime.contains("svg") {
            // SVG is only shown through <img>; opened directly it can't run scripts.
            headers.push(("content-security-policy".into(), "sandbox".into()));
        }
        if variant != "orig" {
            // A derived variant, or (display/thumb not derived yet) the original standing in.
            if let Ok(Some((b, m))) = self.blob_read(hash, variant) {
                headers.push(("content-type".into(), m.unwrap_or_else(|| mime.clone())));
                return (200, headers, b);
            }
            return (
                if variant == "pdf-thumb" { 404 } else { 503 },
                headers,
                b"unavailable".to_vec(),
            );
        }
        let Some(size) = self.blob_size(&h) else {
            return (404, headers, b"unknown blob".to_vec());
        };
        let (a, b, partial) = match range.and_then(|r| parse_range(r, size)) {
            Some((a, b)) => (a, b, true),
            None => (0, size.saturating_sub(1), false),
        };
        if size == 0 {
            headers.push(("content-type".into(), mime));
            return (200, headers, vec![]);
        }
        if !self.blob_is_local(&h) {
            self.blob_want(hash, size, 0);
        }
        match self.blob_range(hash, a, b + 1) {
            Ok(body) => {
                headers.push(("content-type".into(), mime));
                headers.push(("accept-ranges".into(), "bytes".into()));
                if partial {
                    headers.push(("content-range".into(), format!("bytes {a}-{b}/{size}")));
                }
                (if partial { 206 } else { 200 }, headers, body)
            }
            Err(e) => (503, headers, e.into_bytes()),
        }
    }

    /// Drops local bytes the server has confirmed. Never an unconfirmed blob (core enforces it).
    pub fn evict(&self, h: &Hash) -> bool {
        let w = self.inner.st.lock().expect("lock").client.blobs.evict(*h);
        match w {
            Some(w) => {
                self.commit(w);
                let _ = self.inner.blobs.delete(h);
                true
            }
            None => false,
        }
    }

    pub fn set_offline_mode(&self, everything: bool) {
        self.inner.conf.lock().expect("lock").everything = everything;
        self.meta_put(
            "offline",
            Some(if everything {
                "everything"
            } else {
                "on-demand"
            }),
        );
        self.schedule_policy();
    }

    /// Offline attachments (§7.5): everything → queue every missing blob at P3; on demand →
    /// evict to the LRU cap.
    pub(crate) fn schedule_policy(&self) {
        let everything = self.inner.conf.lock().expect("lock").everything;
        if everything {
            let w = {
                let mut st = self.inner.st.lock().expect("lock");
                let wanted: Vec<(Hash, u64)> = st
                    .client
                    .view()
                    .iter()
                    .filter(|e| !e.purged)
                    .filter_map(|e| {
                        let h = e.blob?;
                        let size = st.client.blob_facts(&h)?.size;
                        (!st.client.blobs.is_local(&h)).then_some((h, size))
                    })
                    .collect();
                let now = now_ms();
                wanted
                    .into_iter()
                    .flat_map(|(h, s)| st.client.blobs.want(h, s, 3, now))
                    .collect::<Vec<_>>()
            };
            if !w.is_empty() {
                self.commit(w);
            }
        } else {
            let victims = self
                .inner
                .st
                .lock()
                .expect("lock")
                .client
                .blobs
                .eviction_candidates(ON_DEMAND_CAP);
            for h in victims {
                self.evict(&h);
            }
        }
    }

    // ------------------------------------------------------------------ HTTP helpers

    pub(crate) fn base(&self) -> Result<String, String> {
        self.server()
            .ok_or_else(|| "no server configured".to_string())
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .into()
    }

    pub(crate) fn http_get(
        &self,
        path: &str,
        range: Option<&str>,
    ) -> Result<(u16, Vec<u8>), String> {
        self.http_get_typed_range(path, range)
            .map(|(c, b, _)| (c, b))
    }

    fn http_get_typed(&self, path: &str) -> Result<(u16, Vec<u8>, Option<String>), String> {
        self.http_get_typed_range(path, None)
    }

    fn http_get_typed_range(
        &self,
        path: &str,
        range: Option<&str>,
    ) -> Result<(u16, Vec<u8>, Option<String>), String> {
        let url = format!("{}{path}", self.base()?);
        let mut req = Self::agent().get(&url);
        if let Some(t) = self.token() {
            req = req.header("authorization", &format!("Bearer {t}"));
        }
        if let Some(r) = range {
            req = req.header("range", r);
        }
        let mut resp = req.call().map_err(|e| e.to_string())?;
        let code = resp.status().as_u16();
        let mime = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let body = resp
            .body_mut()
            .with_config()
            .limit(u64::MAX)
            .read_to_vec()
            .map_err(|e| e.to_string())?;
        Ok((code, body, mime))
    }

    /// JSON requests for the auth/devices screens (no CORS on native: they go through Rust).
    pub fn http_json(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        auth: bool,
    ) -> Result<(u16, Value), String> {
        let url = format!("{}{path}", self.base()?);
        let agent = Self::agent();
        let tok = if auth { self.token() } else { None };
        let mut resp = match method {
            "GET" => {
                let mut r = agent.get(&url);
                if let Some(t) = &tok {
                    r = r.header("authorization", &format!("Bearer {t}"));
                }
                r.call()
            }
            _ => {
                let mut r = agent.post(&url);
                if let Some(t) = &tok {
                    r = r.header("authorization", &format!("Bearer {t}"));
                }
                match body {
                    Some(b) => r.send_json(b),
                    None => r.send_empty(),
                }
            }
        }
        .map_err(|e| e.to_string())?;
        let code = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        Ok((code, serde_json::from_str(&text).unwrap_or(Value::Null)))
    }
}
