//! An in-process sync client wired straight to an `Engine` (no network), for import tests.
#![allow(dead_code)]

use jess_core::blobs::{Bitmap, BlobResult, BlobTask};
use jess_core::client::{Client, Output};
use jess_core::kv::Write;
use jess_core::proto::{ClientMsg, ServerMsg, Welcome};
use jess_core::{Hash, Id};
use jess_server::blobfs::BlobFs;
use jess_server::changes::read_changes;
use jess_server::engine::{BeginUpload, Engine};
use std::collections::{BTreeMap, HashMap};

pub struct InProc {
    pub c: Client,
    pub kv: BTreeMap<Vec<u8>, Vec<u8>>,
    pub outbox: Vec<ClientMsg>,
    pub blob_bytes: HashMap<Hash, Vec<u8>>,
    cursor_sent: u64,
}

impl InProc {
    pub fn new(replica: u64) -> InProc {
        let (c, w) = Client::new(replica, jess_core::blobs::CHUNK_SIZE);
        let mut me = InProc {
            c,
            kv: BTreeMap::new(),
            outbox: vec![],
            blob_bytes: HashMap::new(),
            cursor_sent: 0,
        };
        me.commit(w);
        me
    }
    pub fn commit(&mut self, w: Vec<Write>) {
        for x in w {
            match x {
                Write::Put(k, v) => {
                    self.kv.insert(k, v);
                }
                Write::Del(k) => {
                    self.kv.remove(&k);
                }
            }
        }
    }
    pub fn handle(&mut self, o: Output) {
        self.commit(o.writes);
        self.outbox.extend(o.send);
    }
    pub fn connect(&mut self, e: &Engine) {
        let o = self.c.connected("t", "test");
        self.handle(o);
        self.outbox.clear();
        let w = Welcome {
            vault_id: e.vault_id,
            head_seq: e.head,
            server_time: 0,
            min_client_proto: 1,
            hlc: e.hlc(),
        };
        let o = self.c.on_message(ServerMsg::Welcome(w), 1);
        self.handle(o);
        self.cursor_sent = self.c.cursor;
    }
    /// Runs until quiescent: pushes, acks, changes, blob uploads.
    pub fn sync(&mut self, e: &mut Engine, fs: &BlobFs) {
        for _ in 0..10_000 {
            let mut progressed = false;
            for m in std::mem::take(&mut self.outbox) {
                progressed = true;
                match m {
                    ClientMsg::Push { ops } => {
                        let acks = e.push(self.c.replica, None, &ops, 1_000_000).unwrap();
                        let o = self.c.on_message(ServerMsg::Ack { results: acks }, 1);
                        self.handle(o);
                    }
                    ClientMsg::Pull { from, .. } => self.cursor_sent = from,
                    _ => {}
                }
            }
            while self.cursor_sent < e.head {
                let c = read_changes(
                    &e.conn,
                    self.cursor_sent,
                    e.head,
                    self.c.replica,
                    1 << 20,
                    e.hlc(),
                )
                .unwrap();
                self.cursor_sent = c.to;
                let o = self.c.on_message(ServerMsg::Changes(c), 1);
                self.handle(o);
                progressed = true;
            }
            for t in self.c.blob_tasks() {
                progressed = true;
                let r = match t {
                    BlobTask::Begin { hash, size } => {
                        match e.upload_begin(hash, size, None, 0).unwrap() {
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
                            BeginUpload::TooLarge => panic!(),
                        }
                    }
                    BlobTask::PutChunk {
                        hash,
                        upload_id,
                        index,
                        offset,
                        len,
                    } => {
                        let b = &self.blob_bytes[&hash][offset as usize..(offset + len) as usize];
                        fs.write_chunk(&upload_id, offset, b).unwrap();
                        e.upload_chunk_done(&upload_id, index, 0).unwrap();
                        BlobResult::ChunkDone {
                            hash,
                            index,
                            ok: true,
                        }
                    }
                    BlobTask::Complete { hash, upload_id } => {
                        let row = e.upload_get(&upload_id).unwrap().unwrap();
                        let ok = fs.finalize(&upload_id, &hash, row.size).unwrap();
                        e.blob_stored(hash, row.size, Some(&upload_id), 0).unwrap();
                        BlobResult::Completed { hash, ok }
                    }
                    other => panic!("{other:?}"),
                };
                let w = self.c.blob_result(r, 1);
                self.commit(w);
            }
            if !progressed {
                return;
            }
        }
        panic!("in-process sync did not converge");
    }
}

/// `ImportSink` for the client path: local ops through the real sync client.
pub struct ClientSink<'a> {
    pub p: &'a mut InProc,
    pub n: u64,
}

impl jess_core::import::ImportSink for ClientSink<'_> {
    fn new_id(&mut self) -> Id {
        self.n += 1;
        Id::derive(&[b"client-import", &self.n.to_le_bytes()])
    }
    fn meta(&mut self, ops: Vec<jess_core::ops::MetaOp>) -> std::io::Result<()> {
        let o = self
            .p
            .c
            .local_meta(ops, 1_700_000_000_000)
            .map_err(|r| std::io::Error::other(format!("{r:?}")))?;
        self.p.handle(o);
        Ok(())
    }
    fn doc(&mut self, entry: Id, text: &str, _existing: bool) -> std::io::Result<()> {
        let d = jess_core::doc::new_doc(1000 + self.n);
        let u = jess_core::doc::insert(&d, 0, text);
        let o = self
            .p
            .c
            .local_doc_update(entry, "body", u, 1_700_000_000_000);
        self.p.handle(o);
        Ok(())
    }
    fn blob(
        &mut self,
        name: &str,
        r: &mut dyn std::io::Read,
        size: u64,
    ) -> std::io::Result<(Hash, jess_core::model::BlobInfo)> {
        let mut v = Vec::new();
        r.read_to_end(&mut v)?;
        assert_eq!(v.len() as u64, size);
        let h = Hash::of(&v);
        let info = jess_core::import::blob_info_from_header(name, &v[..v.len().min(65536)], size);
        self.p.blob_bytes.insert(h, v);
        let w = self.p.c.blobs.ingest(h, size, 1_700_000_000_000);
        self.p.commit(w);
        Ok((h, info))
    }
}
