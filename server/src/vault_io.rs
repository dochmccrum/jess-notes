//! Server-side import/export glue: a consistent read-only snapshot for export and the mirror,
//! and the `ImportSink` that commits server-authored ops (DESIGN §12).

use crate::blobfs::BlobFs;
use crate::db;
use crate::engine::Engine;
use jess_core::doc as ydoc;
use jess_core::export::ContentSource;
use jess_core::import::{blob_info_from_header, ImportSink};
use jess_core::model::{BlobInfo, SLOT_BODY};
use jess_core::ops::{AckResult, MetaOp};
use jess_core::state::MetaState;
use jess_core::{Hash, Id};
use rusqlite::Connection;
use std::io::{self, Read};

/// A read transaction over `jess.db` (WAL snapshot isolation): everything read through it is
/// one consistent state, and it never blocks the writer.
pub struct Snapshot<'a> {
    conn: &'a Connection,
    fs: &'a BlobFs,
    pub state: MetaState,
    pub head: u64,
}

impl<'a> Snapshot<'a> {
    pub fn begin(conn: &'a Connection, fs: &'a BlobFs) -> rusqlite::Result<Snapshot<'a>> {
        conn.execute_batch("BEGIN")?;
        let head = db::get_meta_i64(conn, "head_seq")?.unwrap_or(0) as u64;
        let state = MetaState::from_entries(db::load_entries(conn)?);
        Ok(Snapshot {
            conn,
            fs,
            state,
            head,
        })
    }

    pub fn doc_rows(&self, id: Id) -> rusqlite::Result<Vec<Vec<u8>>> {
        let mut st = self.conn.prepare_cached(
            "SELECT \"update\" FROM doc_updates WHERE entry_id = ?1 AND slot = ?2 ORDER BY seq",
        )?;
        let v = st
            .query_map(rusqlite::params![id.0.to_vec(), SLOT_BODY], |r| {
                r.get::<_, Vec<u8>>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn text(&self, id: Id) -> io::Result<String> {
        let rows = self.doc_rows(id).map_err(io::Error::other)?;
        let d =
            ydoc::from_updates(1, &rows).map_err(|_| io::Error::other("doc does not decode"))?;
        Ok(ydoc::text(&d))
    }
}

impl Drop for Snapshot<'_> {
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("COMMIT");
    }
}

impl ContentSource for Snapshot<'_> {
    fn text(&mut self, id: Id) -> io::Result<Vec<u8>> {
        Ok(Snapshot::text(self, id)?.into_bytes())
    }
    fn blob(&mut self, hash: Hash) -> io::Result<(Box<dyn Read + '_>, u64)> {
        let f = std::fs::File::open(self.fs.path(&hash))?;
        let n = f.metadata()?.len();
        Ok((Box::new(f), n))
    }
}

/// Access to the engine from an importer: the writer thread in production (each call is one
/// short job, so an import never blocks sync for long), a plain `&mut Engine` in tests.
pub trait EngineAccess {
    fn with<R: Send + 'static>(&mut self, f: impl FnOnce(&mut Engine) -> R + Send + 'static) -> R;
}

impl EngineAccess for &mut Engine {
    fn with<R: Send + 'static>(&mut self, f: impl FnOnce(&mut Engine) -> R + Send + 'static) -> R {
        f(self)
    }
}

impl EngineAccess for crate::writer::Writer {
    fn with<R: Send + 'static>(&mut self, f: impl FnOnce(&mut Engine) -> R + Send + 'static) -> R {
        self.call_blocking(f)
    }
}

pub struct ServerSink<'a, E: EngineAccess> {
    pub engine: E,
    pub fs: &'a BlobFs,
    pub now: u64,
    counter: u64,
    seed: u64,
    pub rejected: Vec<String>,
}

impl<'a, E: EngineAccess> ServerSink<'a, E> {
    pub fn new(engine: E, fs: &'a BlobFs, now: u64, seed: u64) -> Self {
        ServerSink {
            engine,
            fs,
            now,
            counter: 0,
            seed,
            rejected: vec![],
        }
    }
}

struct HeadReader<'r> {
    r: &'r mut dyn Read,
    head: Vec<u8>,
}

impl Read for HeadReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let k = self.r.read(buf)?;
        if self.head.len() < 65536 {
            let take = k.min(65536 - self.head.len());
            self.head.extend_from_slice(&buf[..take]);
        }
        Ok(k)
    }
}

impl<E: EngineAccess> ImportSink for ServerSink<'_, E> {
    fn new_id(&mut self) -> Id {
        self.counter += 1;
        // Unique per (seed, counter); the seed is random per import run.
        let h = Id::derive(&[
            b"import",
            &self.seed.to_le_bytes(),
            &self.counter.to_le_bytes(),
        ]);
        let mut r = [0u8; 10];
        r.copy_from_slice(&h.0[6..16]);
        let mut id = Id::new_v7(self.now, r);
        id.0[7] = h.0[5]; // keep 74 random bits despite the version/variant masks
        id
    }

    fn meta(&mut self, ops: Vec<MetaOp>) -> io::Result<()> {
        let now = self.now;
        let n = ops.len();
        let res = self
            .engine
            .with(move |e| e.server_ops(ops, now).map_err(|e| e.to_string()));
        let acks = res.map_err(io::Error::other)?;
        for (i, (_, a)) in acks.iter().enumerate().take(n) {
            if let AckResult::Rejected(r) = a {
                self.rejected.push(format!("op {i}: {r:?}"));
            }
        }
        Ok(())
    }

    fn doc(&mut self, entry: Id, text: &str, existing: bool) -> io::Result<()> {
        let now = self.now;
        let text = text.to_string();
        let seed = self.seed ^ self.counter;
        self.engine
            .with(move |e| -> Result<(), String> {
                let d = if existing {
                    ydoc::from_updates(
                        seed & 0xffff_ffff | 1,
                        &e.doc_rows(entry, SLOT_BODY).map_err(|e| e.to_string())?,
                    )
                    .map_err(|_| "bad doc".to_string())?
                } else {
                    ydoc::new_doc(seed & 0xffff_ffff | 1)
                };
                let u = if existing {
                    ydoc::set_text(&d, &text)
                } else {
                    ydoc::insert(&d, 0, &text)
                };
                e.server_doc(entry, SLOT_BODY, &u, now)
                    .map_err(|e| e.to_string())?;
                Ok(())
            })
            .map_err(io::Error::other)
    }

    fn blob(&mut self, name: &str, r: &mut dyn Read, size: u64) -> io::Result<(Hash, BlobInfo)> {
        let mut hr = HeadReader {
            r,
            head: Vec::new(),
        };
        let tag = format!("{}-{}", self.seed, self.counter);
        self.counter += 1;
        let (h, n) = self.fs.put_reader(&mut hr, &tag)?;
        if n != size {
            return Err(io::Error::other(format!(
                "{name}: size changed while importing"
            )));
        }
        let now = self.now;
        self.engine
            .with(move |e| e.blob_stored(h, n, None, now).map_err(|e| e.to_string()))
            .map_err(io::Error::other)?;
        Ok((h, blob_info_from_header(name, &hr.head, n)))
    }
}
