//! wasm-bindgen facade over jess-core for the web sync worker (DESIGN D5, §11.2).
//!
//! Everything semantic runs here: the sans-IO sync client, the resolver, extraction, the import
//! planner. The worker supplies I/O (IndexedDB writes, WebSocket frames, blob transfers) and
//! Yjs (for doc merging), exactly like the native hosts do with SQLite and yrs.

use jess_core::blobs::{Bitmap, BlobResult, BlobTask};
use jess_core::client::{Client, Event, Output};
use jess_core::kv::Write;
use jess_core::links::{extract, Syntax};
use jess_core::proto::{self, ClientMsg, HttpSyncRequest, HttpSyncResponse, ServerMsg};
use jess_core::resolve::ResolveIndex;
use jess_core::{Hash, Id};
use js_sys::{Array, Object, Reflect, Uint8Array};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Digest;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

fn set(o: &Object, k: &str, v: &JsValue) {
    let _ = Reflect::set(o, &JsValue::from_str(k), v);
}

fn bytes(b: &[u8]) -> JsValue {
    Uint8Array::from(b).into()
}

fn id_of(s: &str) -> Result<Id, JsValue> {
    Id::parse(s).ok_or_else(|| JsValue::from_str("bad id"))
}

fn hash_of(s: &str) -> Result<Hash, JsValue> {
    Hash::parse_hex(s).ok_or_else(|| JsValue::from_str("bad hash"))
}

fn writes_js(w: Vec<Write>) -> Array {
    let a = Array::new();
    for x in w {
        let pair = Array::new();
        match x {
            Write::Put(k, v) => {
                pair.push(&bytes(&k));
                pair.push(&bytes(&v));
            }
            Write::Del(k) => {
                pair.push(&bytes(&k));
                pair.push(&JsValue::NULL);
            }
        }
        a.push(&pair);
    }
    a
}

fn event_js(e: Event) -> JsValue {
    let o = Object::new();
    match e {
        Event::EntriesChanged(ids) => {
            set(&o, "t", &"entries".into());
            let a = Array::new();
            for i in ids {
                a.push(&i.to_string().into());
            }
            set(&o, "ids", &a);
        }
        Event::DocRemote {
            entry,
            slot,
            update,
        } => {
            set(&o, "t", &"doc".into());
            set(&o, "entry", &entry.to_string().into());
            set(&o, "slot", &slot.into());
            set(&o, "update", &bytes(&update));
        }
        Event::Rejected { op_id, reason } => {
            set(&o, "t", &"rejected".into());
            set(&o, "opId", &(op_id as f64).into());
            set(&o, "reason", &format!("{reason:?}").into());
        }
        Event::Purged(id) => {
            set(&o, "t", &"purged".into());
            set(&o, "id", &id.to_string().into());
        }
        Event::ConnectionDead => set(&o, "t", &"dead".into()),
        Event::Fatal(m) => {
            set(&o, "t", &"fatal".into());
            set(&o, "message", &m.into());
        }
        Event::StatusChanged => set(&o, "t", &"status".into()),
    }
    o.into()
}

fn blob_task_json(t: &BlobTask) -> Value {
    match t {
        BlobTask::Begin { hash, size } => {
            json!({"t": "begin", "hash": hash.to_hex(), "size": size})
        }
        BlobTask::PutChunk {
            hash,
            upload_id,
            index,
            offset,
            len,
        } => {
            json!({"t": "put", "hash": hash.to_hex(), "uploadId": upload_id, "index": index, "offset": offset, "len": len})
        }
        BlobTask::Complete { hash, upload_id } => {
            json!({"t": "complete", "hash": hash.to_hex(), "uploadId": upload_id})
        }
        BlobTask::GetRange {
            hash,
            index,
            offset,
            len,
        } => {
            json!({"t": "get", "hash": hash.to_hex(), "index": index, "offset": offset, "len": len})
        }
        BlobTask::Presence { hashes } => {
            json!({"t": "presence", "hashes": hashes.iter().map(|h| h.to_hex()).collect::<Vec<_>>()})
        }
    }
}

fn blob_task_from(v: &Value) -> Option<BlobTask> {
    let h = Hash::parse_hex(v.get("hash").and_then(|x| x.as_str()).unwrap_or(""));
    Some(match v.get("t")?.as_str()? {
        "begin" => BlobTask::Begin {
            hash: h?,
            size: v["size"].as_u64()?,
        },
        "put" => BlobTask::PutChunk {
            hash: h?,
            upload_id: v["uploadId"].as_str()?.into(),
            index: v["index"].as_u64()? as u32,
            offset: v["offset"].as_u64()?,
            len: v["len"].as_u64()?,
        },
        "complete" => BlobTask::Complete {
            hash: h?,
            upload_id: v["uploadId"].as_str()?.into(),
        },
        "get" => BlobTask::GetRange {
            hash: h?,
            index: v["index"].as_u64()? as u32,
            offset: v["offset"].as_u64()?,
            len: v["len"].as_u64()?,
        },
        "presence" => BlobTask::Presence {
            hashes: v["hashes"]
                .as_array()?
                .iter()
                .filter_map(|x| Hash::parse_hex(x.as_str()?))
                .collect(),
        },
        _ => return None,
    })
}

/// Parses a blob transfer result from the worker.
fn blob_result_from(v: &Value) -> Result<BlobResult, JsValue> {
    let h = || {
        Hash::parse_hex(v["hash"].as_str().unwrap_or(""))
            .ok_or_else(|| JsValue::from_str("bad hash"))
    };
    Ok(match v["t"].as_str().unwrap_or("") {
        "began" => BlobResult::Began {
            hash: h()?,
            present: v["present"].as_bool().unwrap_or(false),
            upload_id: v["uploadId"].as_str().map(String::from),
            received: Bitmap(hex::decode(v["received"].as_str().unwrap_or("")).unwrap_or_default()),
        },
        "chunk" => BlobResult::ChunkDone {
            hash: h()?,
            index: v["index"].as_u64().unwrap_or(0) as u32,
            ok: v["ok"].as_bool().unwrap_or(false),
        },
        "completed" => BlobResult::Completed {
            hash: h()?,
            ok: v["ok"].as_bool().unwrap_or(false),
        },
        "gone" => BlobResult::UploadGone { hash: h()? },
        "range" => BlobResult::RangeDone {
            hash: h()?,
            index: v["index"].as_u64().unwrap_or(0) as u32,
            ok: v["ok"].as_bool().unwrap_or(false),
        },
        "presence" => {
            let list = |k: &str| {
                v[k].as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| Hash::parse_hex(x.as_str()?))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            BlobResult::Presence {
                present: list("present"),
                missing: list("missing"),
            }
        }
        "failed" => BlobResult::Failed {
            task: blob_task_from(&v["task"]).ok_or_else(|| JsValue::from_str("bad task"))?,
        },
        other => return Err(JsValue::from_str(&format!("unknown blob result {other}"))),
    })
}

/// The worker's handle on the sync client.
#[wasm_bindgen]
pub struct Core {
    c: Client,
    ix: ResolveIndex,
}

#[wasm_bindgen]
impl Core {
    /// Loads from persisted KV pairs. Returns the core; call `take_init_writes` once.
    #[wasm_bindgen(constructor)]
    pub fn new(keys: Array, vals: Array, new_replica_hex: &str, chunk: f64) -> Core {
        let mut items = Vec::with_capacity(keys.length() as usize);
        for i in 0..keys.length() {
            let k = Uint8Array::from(keys.get(i)).to_vec();
            let v = Uint8Array::from(vals.get(i)).to_vec();
            items.push((k, v));
        }
        let r = u64::from_str_radix(new_replica_hex, 16).unwrap_or(1).max(1);
        let (c, w) = Client::load(items, r, chunk as u64);
        INIT.with(|x| *x.borrow_mut() = w);
        let ix = ResolveIndex::build(c.view());
        Core { c, ix }
    }

    /// Writes produced by `new` (a fresh replica id), to commit once.
    pub fn take_init_writes(&self) -> Array {
        writes_js(INIT.with(|x| std::mem::take(&mut *x.borrow_mut())))
    }

    fn out(&mut self, o: Output) -> JsValue {
        let obj = Object::new();
        let changed: Vec<Id> = o
            .events
            .iter()
            .filter_map(|e| {
                if let Event::EntriesChanged(ids) = e {
                    Some(ids.clone())
                } else {
                    None
                }
            })
            .flatten()
            .collect();
        if !changed.is_empty() {
            self.ix.sync(self.c.view(), &changed);
        }
        set(&obj, "writes", &writes_js(o.writes));
        let s = Array::new();
        for m in o.send {
            s.push(&bytes(&proto::encode(&m)));
        }
        set(&obj, "send", &s);
        let ev = Array::new();
        for e in o.events {
            ev.push(&event_js(e));
        }
        set(&obj, "events", &ev);
        obj.into()
    }

    #[wasm_bindgen(getter)]
    pub fn replica(&self) -> String {
        format!("{:x}", self.c.replica)
    }
    #[wasm_bindgen(getter)]
    pub fn cursor(&self) -> f64 {
        self.c.cursor as f64
    }
    #[wasm_bindgen(getter, js_name = vaultId)]
    pub fn vault_id(&self) -> Option<String> {
        self.c.vault.map(|v| v.to_string())
    }

    pub fn connected(&mut self, token: &str, version: &str) -> JsValue {
        let o = self.c.connected(token, version);
        self.out(o)
    }
    pub fn disconnected(&mut self) -> JsValue {
        let o = self.c.disconnected();
        self.out(o)
    }
    /// A binary WebSocket frame from the server.
    pub fn frame(&mut self, frame: &[u8], now: f64) -> Result<JsValue, JsValue> {
        let m: ServerMsg = proto::decode(frame).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let o = self.c.on_message(m, now as u64);
        Ok(self.out(o))
    }
    pub fn tick(&mut self, now: f64) -> JsValue {
        let o = self.c.tick(now as u64);
        self.out(o)
    }
    pub fn probe(&mut self, now: f64) -> Uint8Array {
        Uint8Array::from(&proto::encode(&self.c.probe(now as u64))[..])
    }

    /// Meta intents (JSON array of ops); several ops form one atomic group.
    #[wasm_bindgen(js_name = localMeta)]
    pub fn local_meta(&mut self, ops_json: &str, now: f64) -> Result<JsValue, JsValue> {
        let ops = jess_core::json::parse_ops(ops_json).map_err(|e| JsValue::from_str(&e))?;
        match self.c.local_meta(ops, now as u64) {
            Ok(o) => Ok(self.out(o)),
            Err(r) => Err(JsValue::from_str(&format!("{r:?}"))),
        }
    }

    #[wasm_bindgen(js_name = localDocUpdate)]
    pub fn local_doc_update(
        &mut self,
        entry: &str,
        slot: &str,
        update: &[u8],
        now: f64,
    ) -> Result<JsValue, JsValue> {
        let o = self
            .c
            .local_doc_update(id_of(entry)?, slot, update.to_vec(), now as u64);
        Ok(self.out(o))
    }

    #[wasm_bindgen(js_name = compactDoc)]
    pub fn compact_doc(
        &mut self,
        entry: &str,
        slot: &str,
        merged: &[u8],
        upto: f64,
    ) -> Result<Array, JsValue> {
        Ok(writes_js(self.c.compact_doc(
            id_of(entry)?,
            slot,
            merged.to_vec(),
            upto as u64,
        )))
    }

    /// KV prefix of a doc's confirmed rows (scan it to read them in order).
    #[wasm_bindgen(js_name = docPrefix)]
    pub fn doc_prefix(&self, entry: &str, slot: &str) -> Result<Uint8Array, JsValue> {
        Ok(Uint8Array::from(
            &jess_core::kv::doc_prefix(id_of(entry)?, slot)[..],
        ))
    }

    #[wasm_bindgen(js_name = docSeqs)]
    pub fn doc_seqs(&self, entry: &str, slot: &str) -> Result<Array, JsValue> {
        let a = Array::new();
        for s in self.c.doc_seqs(id_of(entry)?, slot) {
            a.push(&(s as f64).into());
        }
        Ok(a)
    }

    #[wasm_bindgen(js_name = pendingDocUpdates)]
    pub fn pending_doc_updates(&self, entry: &str, slot: &str) -> Result<Array, JsValue> {
        let a = Array::new();
        for u in self.c.pending_doc_updates(id_of(entry)?, slot) {
            a.push(&bytes(&u));
        }
        Ok(a)
    }

    /// All entries of the optimistic view (JSON array).
    #[wasm_bindgen(js_name = viewJson)]
    pub fn view_json(&self) -> String {
        jess_core::json::view_json(&self.c)
    }

    #[wasm_bindgen(js_name = entriesJson)]
    pub fn entries_json(&self, ids: Array) -> String {
        let ids: Vec<Id> = (0..ids.length())
            .filter_map(|i| ids.get(i).as_string().and_then(|s| Id::parse(&s)))
            .collect();
        serde_json::to_string(&jess_core::json::entries_view(&self.c, &ids)).unwrap_or_default()
    }

    #[wasm_bindgen(js_name = pathOf)]
    pub fn path_of(&self, id: &str) -> Option<String> {
        self.c.view().path_of(Id::parse(id)?)
    }

    /// Resolves a link from note `src` (redirect overlay included).
    pub fn resolve(&self, target: &str, markdown: bool, src: Option<String>) -> Option<String> {
        let folder = src
            .and_then(|s| Id::parse(&s))
            .map(|s| self.c.view().folder_of(s))
            .unwrap_or_default();
        let syn = if markdown {
            Syntax::Markdown
        } else {
            Syntax::Wiki
        };
        self.c
            .redirects()
            .resolve(&self.ix, target, syn, &folder)
            .map(|r| r.id.to_string())
    }

    #[wasm_bindgen(js_name = statusJson)]
    pub fn status_json(&self) -> String {
        jess_core::json::status(&self.c).to_string()
    }

    #[wasm_bindgen(js_name = quarantineJson)]
    pub fn quarantine_json(&self) -> String {
        serde_json::to_string(&jess_core::json::quarantine(&self.c)).unwrap_or_default()
    }

    // ------------------------------------------------------------------ blobs

    #[wasm_bindgen(js_name = blobTasks)]
    pub fn blob_tasks(&mut self) -> String {
        let t: Vec<Value> = self.c.blob_tasks().iter().map(blob_task_json).collect();
        serde_json::to_string(&t).unwrap_or_default()
    }
    #[wasm_bindgen(js_name = presenceTask)]
    pub fn presence_task(&mut self) -> Option<String> {
        self.c
            .blobs
            .presence_task()
            .map(|t| blob_task_json(&t).to_string())
    }
    #[wasm_bindgen(js_name = blobResult)]
    pub fn blob_result(&mut self, result_json: &str, now: f64) -> Result<Array, JsValue> {
        let v: Value =
            serde_json::from_str(result_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let r = blob_result_from(&v)?;
        Ok(writes_js(self.c.blob_result(r, now as u64)))
    }
    #[wasm_bindgen(js_name = blobIngest)]
    pub fn blob_ingest(&mut self, hash: &str, size: f64, now: f64) -> Result<Array, JsValue> {
        Ok(writes_js(self.c.blobs.ingest(
            hash_of(hash)?,
            size as u64,
            now as u64,
        )))
    }
    #[wasm_bindgen(js_name = blobWant)]
    pub fn blob_want(
        &mut self,
        hash: &str,
        size: f64,
        prio: u8,
        now: f64,
    ) -> Result<Array, JsValue> {
        Ok(writes_js(self.c.blobs.want(
            hash_of(hash)?,
            size as u64,
            prio,
            now as u64,
        )))
    }
    #[wasm_bindgen(js_name = blobIsLocal)]
    pub fn blob_is_local(&self, hash: &str) -> bool {
        Hash::parse_hex(hash)
            .map(|h| self.c.blobs.is_local(&h))
            .unwrap_or(false)
    }
    #[wasm_bindgen(js_name = blobEvictionCandidates)]
    pub fn blob_eviction_candidates(&self, cap: f64) -> Array {
        let a = Array::new();
        for h in self.c.blobs.eviction_candidates(cap as u64) {
            a.push(&h.to_hex().into());
        }
        a
    }
    /// Evicts local bytes (refused unless the server has confirmed the blob).
    #[wasm_bindgen(js_name = blobEvict)]
    pub fn blob_evict(&mut self, hash: &str) -> Option<Array> {
        self.c.blobs.evict(Hash::parse_hex(hash)?).map(writes_js)
    }

    // ------------------------------------------------------------------ HTTP fallback

    /// Builds a `POST /api/sync` body from frames produced by `connected` / local ops.
    #[wasm_bindgen(js_name = httpRequest)]
    pub fn http_request(&self, frames: Array, wait_s: u32) -> Result<Uint8Array, JsValue> {
        let mut hello = None;
        let mut ops = Vec::new();
        for i in 0..frames.length() {
            let b = Uint8Array::from(frames.get(i)).to_vec();
            match proto::decode::<ClientMsg>(&b) {
                Ok(ClientMsg::Hello(h)) => hello = Some(h),
                Ok(ClientMsg::Push { ops: o }) => ops.extend(o),
                _ => {}
            }
        }
        let hello = hello.ok_or_else(|| JsValue::from_str("missing hello"))?;
        let req = HttpSyncRequest {
            cursor: self.c.cursor,
            hello,
            ops,
            wait_s,
        };
        Ok(Uint8Array::from(&proto::encode(&req)[..]))
    }

    /// Feeds a `POST /api/sync` response (welcome, acks, changes) into the client.
    #[wasm_bindgen(js_name = httpResponse)]
    pub fn http_response(&mut self, body: &[u8], now: f64) -> Result<JsValue, JsValue> {
        let r: HttpSyncResponse =
            proto::decode(body).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let mut all = Output::default();
        let mut feed = |m: ServerMsg, c: &mut Client| {
            let o = c.on_message(m, now as u64);
            all.writes.extend(o.writes);
            all.send.extend(o.send);
            all.events.extend(o.events);
        };
        feed(ServerMsg::Welcome(r.welcome), &mut self.c);
        if !r.acks.is_empty() {
            feed(ServerMsg::Ack { results: r.acks }, &mut self.c);
        }
        for ch in r.changes {
            feed(ServerMsg::Changes(ch), &mut self.c);
        }
        Ok(self.out(all))
    }
}

thread_local! {
    static INIT: std::cell::RefCell<Vec<Write>> = const { std::cell::RefCell::new(Vec::new()) };
}

// ---------------------------------------------------------------------- free functions

#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Links, tags, maths and frontmatter of a note, as JSON.
#[wasm_bindgen(js_name = extract)]
pub fn extract_json(text: &str) -> String {
    serde_json::to_string(&extract(text)).unwrap_or_default()
}

/// A new entry id (UUIDv7) from a timestamp and 10 random bytes.
#[wasm_bindgen(js_name = newId)]
pub fn new_id(now: f64, rand: &[u8]) -> String {
    let mut r = [0u8; 10];
    r.copy_from_slice(
        &rand[..10.min(rand.len())]
            .iter()
            .chain(std::iter::repeat(&0))
            .take(10)
            .copied()
            .collect::<Vec<_>>(),
    );
    Id::new_v7(now as u64, r).to_string()
}

/// Streaming SHA-256 for blob ingest.
#[wasm_bindgen]
pub struct Sha256 {
    h: sha2::Sha256,
}

#[wasm_bindgen]
impl Sha256 {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Sha256 {
        Sha256 {
            h: sha2::Sha256::new(),
        }
    }
    pub fn update(&mut self, chunk: &[u8]) {
        self.h.update(chunk);
    }
    pub fn finish(self) -> String {
        hex::encode(self.h.finalize())
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

/// Blob facts from a file's first bytes (image dimensions, EXIF orientation, mime).
#[wasm_bindgen(js_name = blobInfo)]
pub fn blob_info(name: &str, header: &[u8], size: f64) -> String {
    jess_core::json::blob_info(&jess_core::import::blob_info_from_header(
        name,
        header,
        size as u64,
    ))
    .to_string()
}

#[wasm_bindgen(js_name = validateName)]
pub fn validate_name(name: &str) -> Option<String> {
    jess_core::names::validate_new_name(name)
        .err()
        .map(|e| format!("{e:?}"))
}

// ---------------------------------------------------------------------- import / export

/// An import source backed by JS: `{ files: [{path, size, mtime?, ctime?}], dirs: [..],
/// read(path): Uint8Array }` (a folder picked with webkitdirectory), or a zip: `{ size,
/// readAt(offset, len): Uint8Array }`. Reads are synchronous (FileReaderSync in the worker).
struct JsFolder {
    obj: JsValue,
}

fn get(o: &JsValue, k: &str) -> JsValue {
    Reflect::get(o, &JsValue::from_str(k)).unwrap_or(JsValue::UNDEFINED)
}

fn call1(o: &JsValue, f: &str, a: &JsValue) -> Result<JsValue, JsValue> {
    let func: js_sys::Function = get(o, f).dyn_into()?;
    func.call1(o, a)
}

fn call2(o: &JsValue, f: &str, a: &JsValue, b: &JsValue) -> Result<JsValue, JsValue> {
    let func: js_sys::Function = get(o, f).dyn_into()?;
    func.call2(o, a, b)
}

fn io_err(e: JsValue) -> std::io::Error {
    std::io::Error::other(e.as_string().unwrap_or_else(|| "js error".into()))
}

impl jess_core::import::ImportSource for JsFolder {
    fn list(&mut self) -> std::io::Result<(Vec<jess_core::import::SourceFile>, Vec<String>)> {
        let files: Array = get(&self.obj, "files").dyn_into().map_err(io_err)?;
        let mut out = Vec::new();
        for f in files.iter() {
            out.push(jess_core::import::SourceFile {
                path: get(&f, "path").as_string().unwrap_or_default(),
                size: get(&f, "size").as_f64().unwrap_or(0.0) as u64,
                mtime: get(&f, "mtime").as_f64().map(|v| v as u64),
                ctime: get(&f, "ctime").as_f64().map(|v| v as u64),
                compressed: None,
            });
        }
        let dirs: Vec<String> = get(&self.obj, "dirs")
            .dyn_into::<Array>()
            .map(|a| a.iter().filter_map(|d| d.as_string()).collect())
            .unwrap_or_default();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok((out, dirs))
    }
    fn open(&mut self, path: &str) -> std::io::Result<Box<dyn std::io::Read + '_>> {
        let v = call1(&self.obj, "read", &JsValue::from_str(path)).map_err(io_err)?;
        Ok(Box::new(std::io::Cursor::new(Uint8Array::from(v).to_vec())))
    }
}

/// `Read + Seek` over a JS blob reader (`size`, `readAt(offset, len)`), for zip archives.
struct JsBlobReader {
    obj: JsValue,
    pos: u64,
    size: u64,
}

impl std::io::Read for JsBlobReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.size || buf.is_empty() {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(self.size - self.pos).min(4 << 20);
        let v = call2(
            &self.obj,
            "readAt",
            &(self.pos as f64).into(),
            &(n as f64).into(),
        )
        .map_err(io_err)?;
        let a = Uint8Array::from(v);
        let k = (a.length() as usize).min(buf.len());
        a.subarray(0, k as u32).copy_to(&mut buf[..k]);
        self.pos += k as u64;
        Ok(k)
    }
}

impl std::io::Seek for JsBlobReader {
    fn seek(&mut self, p: std::io::SeekFrom) -> std::io::Result<u64> {
        let np = match p {
            std::io::SeekFrom::Start(x) => x as i64,
            std::io::SeekFrom::End(x) => self.size as i64 + x,
            std::io::SeekFrom::Current(x) => self.pos as i64 + x,
        };
        if np < 0 {
            return Err(std::io::Error::other("seek before start"));
        }
        self.pos = np as u64;
        Ok(self.pos)
    }
}

fn with_source<R>(
    src: JsValue,
    f: impl FnOnce(&mut dyn jess_core::import::ImportSource) -> std::io::Result<R>,
) -> Result<R, JsValue> {
    let is_zip = get(&src, "readAt").is_function();
    let r = if is_zip {
        let size = get(&src, "size").as_f64().unwrap_or(0.0) as u64;
        let reader = std::io::BufReader::with_capacity(
            1 << 20,
            JsBlobReader {
                obj: src,
                pos: 0,
                size,
            },
        );
        let mut z = jess_core::import::ZipSource::new(reader)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        f(&mut z)
    } else {
        let mut s = JsFolder { obj: src };
        f(&mut s)
    };
    r.map_err(|e| JsValue::from_str(&e.to_string()))
}

struct MapExisting<'a> {
    st: &'a jess_core::state::MetaState,
    texts: std::collections::HashMap<Id, Vec<u8>>,
}

impl jess_core::import::Existing for MapExisting<'_> {
    fn state(&self) -> &jess_core::state::MetaState {
        self.st
    }
    fn text(&mut self, id: Id) -> Option<Vec<u8>> {
        self.texts.get(&id).cloned()
    }
}

#[wasm_bindgen]
impl Core {
    /// Existing markdown notes at paths the source also has: the host must supply their texts
    /// to `importPlan` (idempotency).
    #[wasm_bindgen(js_name = importCollisions)]
    pub fn import_collisions(&self, src: JsValue) -> Result<Array, JsValue> {
        let view = self.c.view();
        let files = with_source(src, |s| s.list())?.0;
        let out = Array::new();
        for f in files {
            if let Some(id) = view.by_path(&f.path) {
                if view
                    .get(&id)
                    .map(|e| e.kind == "markdown" && e.blob.is_none())
                    .unwrap_or(false)
                {
                    out.push(&id.to_string().into());
                }
            }
        }
        Ok(out)
    }

    /// Plans an import (also the dry run). `texts` maps existing note ids to their exact text.
    #[wasm_bindgen(js_name = importPlan)]
    pub fn import_plan(
        &self,
        src: JsValue,
        texts: js_sys::Map,
        opts_json: &str,
    ) -> Result<String, JsValue> {
        #[derive(Deserialize, Default)]
        #[serde(rename_all = "camelCase")]
        struct O {
            hide_pdfs_in_attachment_folder: Option<bool>,
            conflict: Option<String>,
        }
        let o: O = serde_json::from_str(opts_json).unwrap_or_default();
        let conflict = match o.conflict.as_deref() {
            Some("overwrite") => jess_core::import::Conflict::Overwrite,
            Some("keepBoth") => jess_core::import::Conflict::KeepBoth,
            Some("skip") => jess_core::import::Conflict::Skip,
            _ => jess_core::import::Conflict::Ask,
        };
        let opts = jess_core::import::PlanOptions {
            conflict,
            hide_pdfs_in_attachment_folder: o.hide_pdfs_in_attachment_folder.unwrap_or(true),
            ..Default::default()
        };
        let mut m = std::collections::HashMap::new();
        texts.for_each(&mut |v, k| {
            if let (Some(id), Ok(b)) = (
                k.as_string().and_then(|s| Id::parse(&s)),
                v.dyn_into::<Uint8Array>(),
            ) {
                m.insert(id, b.to_vec());
            }
        });
        let mut ex = MapExisting {
            st: self.c.view(),
            texts: m,
        };
        let plan = with_source(src, |s| jess_core::import::plan(s, &mut ex, &opts))?;
        serde_json::to_string(&plan).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// The live entry at an exact vault path.
    #[wasm_bindgen(js_name = idByPath)]
    pub fn id_by_path(&self, path: &str) -> Option<String> {
        self.c.view().by_path(path).map(|i| i.to_string())
    }

    /// The export projection: `{items: [{id, path, kind: dir|text|blob, hash?, modified?}], report, mapped}`.
    pub fn projection(&self, portable: bool, include_trash: bool) -> String {
        use jess_core::projection::{project, Options, Profile, Source};
        let p = project(
            self.c.view(),
            Options {
                profile: if portable {
                    Profile::Portable
                } else {
                    Profile::Exact
                },
                include_trash,
            },
        );
        let items: Vec<Value> = p
            .items
            .iter()
            .map(|i| match &i.source {
                Source::Dir => json!({"id": i.id.to_string(), "path": i.path, "kind": "dir", "modified": i.modified_at}),
                Source::Text(id) => json!({"id": id.to_string(), "path": i.path, "kind": "text", "modified": i.modified_at}),
                Source::Blob(h) => json!({"id": i.id.to_string(), "path": i.path, "kind": "blob", "hash": h.to_hex(), "modified": i.modified_at}),
            })
            .collect();
        let report = if p.mapped.is_empty() && p.errors.is_empty() {
            Value::Null
        } else {
            Value::String(p.report())
        };
        json!({"items": items, "report": report}).to_string()
    }
}

/// Reads entries of a zip (same path normalisation as the import planner).
#[wasm_bindgen]
pub struct ZipReader {
    z: jess_core::import::ZipSource<std::io::BufReader<JsBlobReader>>,
}

#[wasm_bindgen]
impl ZipReader {
    #[wasm_bindgen(constructor)]
    pub fn new(src: JsValue) -> Result<ZipReader, JsValue> {
        use jess_core::import::ImportSource;
        let size = get(&src, "size").as_f64().unwrap_or(0.0) as u64;
        let reader = std::io::BufReader::with_capacity(
            1 << 20,
            JsBlobReader {
                obj: src,
                pos: 0,
                size,
            },
        );
        let mut z = jess_core::import::ZipSource::new(reader)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        z.list().map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(ZipReader { z })
    }
    pub fn read(&mut self, path: &str) -> Result<Uint8Array, JsValue> {
        use jess_core::import::ImportSource;
        let v = self
            .z
            .read_all(path)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(Uint8Array::from(&v[..]))
    }
}
