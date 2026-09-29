//! `app.db` (DESIGN §4.2): the client's source of truth on native platforms. The sync client
//! persists itself as ordered key-value writes (the same keys as IndexedDB on the web), so the
//! store is a `kv` table plus a small `meta` table for host things (token, server, boot record).

use jess_core::kv::Write;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Store> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS kv(k BLOB PRIMARY KEY, v BLOB NOT NULL) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v BLOB);",
        )?;
        Ok(Store { conn })
    }

    /// Everything, in key order (to rebuild the client).
    pub fn load(&self) -> rusqlite::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut st = self.conn.prepare("SELECT k, v FROM kv ORDER BY k")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    /// Commits writes atomically, in order.
    pub fn commit(&mut self, writes: &[Write]) -> rusqlite::Result<()> {
        if writes.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut put = tx.prepare_cached("INSERT OR REPLACE INTO kv(k, v) VALUES (?1, ?2)")?;
            let mut del = tx.prepare_cached("DELETE FROM kv WHERE k = ?1")?;
            for w in writes {
                match w {
                    Write::Put(k, v) => {
                        put.execute(params![k, v])?;
                    }
                    Write::Del(k) => {
                        del.execute(params![k])?;
                    }
                }
            }
        }
        tx.commit()
    }

    /// Values under a key prefix, in key order.
    pub fn scan_prefix(&self, prefix: &[u8]) -> rusqlite::Result<Vec<Vec<u8>>> {
        let mut upper = prefix.to_vec();
        let bounded = loop {
            match upper.last_mut() {
                None => break false,
                Some(b) if *b == 0xff => {
                    upper.pop();
                }
                Some(b) => {
                    *b += 1;
                    break true;
                }
            }
        };
        if bounded {
            let mut st = self
                .conn
                .prepare_cached("SELECT v FROM kv WHERE k >= ?1 AND k < ?2 ORDER BY k")?;
            let rows = st.query_map(params![prefix, upper], |r| r.get(0))?;
            rows.collect()
        } else {
            let mut st = self
                .conn
                .prepare_cached("SELECT v FROM kv WHERE k >= ?1 ORDER BY k")?;
            let rows = st.query_map(params![prefix], |r| r.get(0))?;
            rows.collect()
        }
    }

    pub fn meta_get(&self, k: &str) -> rusqlite::Result<Option<Vec<u8>>> {
        self.conn
            .query_row("SELECT v FROM meta WHERE k = ?1", [k], |r| r.get(0))
            .optional()
    }

    pub fn meta_put(&self, k: &str, v: Option<&[u8]>) -> rusqlite::Result<()> {
        match v {
            Some(v) => self.conn.execute(
                "INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)",
                params![k, v],
            ),
            None => self.conn.execute("DELETE FROM meta WHERE k = ?1", [k]),
        }
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kv_order_prefix_and_atomicity() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(&dir.path().join("app.db")).unwrap();
        s.commit(&[
            Write::Put(vec![2, 0], vec![20]),
            Write::Put(vec![1, 255], vec![19]),
            Write::Put(vec![1], vec![1]),
            Write::Put(vec![0xff, 1], vec![4]),
            Write::Put(vec![0xff, 0xff], vec![5]),
        ])
        .unwrap();
        s.commit(&[Write::Del(vec![1])]).unwrap();
        let all = s.load().unwrap();
        assert_eq!(
            all.iter().map(|x| x.0.clone()).collect::<Vec<_>>(),
            vec![vec![1, 255], vec![2, 0], vec![0xff, 1], vec![0xff, 0xff]]
        );
        assert_eq!(s.scan_prefix(&[1]).unwrap(), vec![vec![19]]);
        assert_eq!(s.scan_prefix(&[0xff]).unwrap(), vec![vec![4], vec![5]]);
        s.meta_put("token", Some(b"t")).unwrap();
        assert_eq!(s.meta_get("token").unwrap().as_deref(), Some(&b"t"[..]));
        s.meta_put("token", None).unwrap();
        assert_eq!(s.meta_get("token").unwrap(), None);
    }
}
