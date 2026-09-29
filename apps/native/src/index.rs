//! `index.db` (DESIGN §4.2): links, tags and FTS5 over notes and PDF pages. Derived data: it can
//! be deleted and rebuilt at any time. Same schema and queries as the web's sqlite-wasm index.

use jess_core::links::Extracted;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT);
CREATE TABLE IF NOT EXISTS notes(id TEXT PRIMARY KEY, hash TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS pdfs(id TEXT PRIMARY KEY, blob TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS links(src TEXT NOT NULL, ord INTEGER NOT NULL, target TEXT NOT NULL, tkey TEXT NOT NULL, markdown INTEGER NOT NULL, embed INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS links_src ON links(src);
CREATE INDEX IF NOT EXISTS links_key ON links(tkey);
CREATE TABLE IF NOT EXISTS tags(src TEXT NOT NULL, name TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS tags_src ON tags(src);
CREATE INDEX IF NOT EXISTS tags_name ON tags(name);
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(id UNINDEXED, page UNINDEXED, name, body, tokenize = 'unicode61 remove_diacritics 2');
";

/// The key links are looked up by: the last path segment, NFC, lower-cased, without `.md`.
pub fn target_key(target: &str) -> String {
    let t = target.trim();
    let base = t.rsplit('/').next().unwrap_or(t).trim();
    let k = jess_core::names::lookup_key(base);
    k.strip_suffix(".md").map(str::to_string).unwrap_or(k)
}

pub struct IndexedLink {
    pub src: String,
    pub target: String,
    pub markdown: bool,
    pub embed: bool,
}

pub struct Index {
    db: Connection,
}

impl Index {
    pub fn open(path: &Path) -> rusqlite::Result<Index> {
        let db = Connection::open(path)?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        let version: i64 = db
            .query_row("SELECT v FROM meta WHERE k = 'schema'", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .unwrap_or(None)
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if version != SCHEMA_VERSION {
            for t in ["fts", "links", "tags", "notes", "pdfs", "meta"] {
                db.execute_batch(&format!("DROP TABLE IF EXISTS {t}"))?;
            }
        }
        db.execute_batch(SCHEMA)?;
        db.execute(
            "INSERT OR REPLACE INTO meta(k, v) VALUES ('schema', ?1)",
            [SCHEMA_VERSION.to_string()],
        )?;
        Ok(Index { db })
    }

    pub fn indexed_hashes(&self) -> rusqlite::Result<HashMap<String, String>> {
        let mut st = self.db.prepare("SELECT id, hash FROM notes")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    pub fn indexed_pdfs(&self) -> rusqlite::Result<HashMap<String, String>> {
        let mut st = self.db.prepare("SELECT id, blob FROM pdfs")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    /// Re-indexes one note; false if its name and text are unchanged.
    pub fn upsert(
        &mut self,
        id: &str,
        name: &str,
        text: &str,
        ex: &Extracted,
    ) -> rusqlite::Result<bool> {
        let mut h = Sha256::new();
        h.update(name.as_bytes());
        h.update([0]);
        h.update(text.as_bytes());
        let hash = hex::encode(h.finalize());
        let cur: Option<String> = self
            .db
            .query_row("SELECT hash FROM notes WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        if cur.as_deref() == Some(hash.as_str()) {
            return Ok(false);
        }
        let tx = self.db.transaction()?;
        for t in ["links", "tags"] {
            tx.execute(&format!("DELETE FROM {t} WHERE src = ?1"), [id])?;
        }
        tx.execute("DELETE FROM fts WHERE id = ?1", [id])?;
        for (i, l) in ex.links.iter().enumerate() {
            if l.target.trim().is_empty() {
                continue;
            }
            tx.execute(
                "INSERT INTO links(src, ord, target, tkey, markdown, embed) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, i as i64, l.target, target_key(&l.target), l.syntax == jess_core::links::Syntax::Markdown, l.embed],
            )?;
        }
        let mut seen = std::collections::BTreeSet::new();
        for t in &ex.tags {
            if seen.insert(t.name.to_lowercase()) {
                tx.execute(
                    "INSERT INTO tags(src, name) VALUES (?1, ?2)",
                    params![id, t.name.to_lowercase()],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO fts(id, page, name, body) VALUES (?1, NULL, ?2, ?3)",
            params![id, name, text],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO notes(id, hash) VALUES (?1, ?2)",
            params![id, hash],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn upsert_pdf(
        &mut self,
        id: &str,
        name: &str,
        blob: &str,
        pages: &[String],
    ) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM fts WHERE id = ?1", [id])?;
        for (i, t) in pages.iter().enumerate() {
            if !t.trim().is_empty() {
                tx.execute(
                    "INSERT INTO fts(id, page, name, body) VALUES (?1, ?2, ?3, ?4)",
                    params![id, (i + 1) as i64, name, t],
                )?;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO pdfs(id, blob) VALUES (?1, ?2)",
            params![id, blob],
        )?;
        tx.commit()
    }

    pub fn remove(&mut self, id: &str) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        for t in ["links", "tags"] {
            tx.execute(&format!("DELETE FROM {t} WHERE src = ?1"), [id])?;
        }
        for t in ["fts", "notes", "pdfs"] {
            tx.execute(&format!("DELETE FROM {t} WHERE id = ?1"), [id])?;
        }
        tx.commit()
    }

    pub fn links_by_keys(&self, keys: &[String]) -> rusqlite::Result<Vec<IndexedLink>> {
        let mut out = Vec::new();
        let mut st = self
            .db
            .prepare_cached("SELECT src, target, markdown, embed FROM links WHERE tkey = ?1")?;
        for k in keys {
            let rows = st.query_map([k], |r| {
                Ok(IndexedLink {
                    src: r.get(0)?,
                    target: r.get(1)?,
                    markdown: r.get(2)?,
                    embed: r.get(3)?,
                })
            })?;
            for r in rows {
                out.push(r?);
            }
        }
        Ok(out)
    }

    pub fn tags(&self) -> rusqlite::Result<Vec<Value>> {
        let mut st = self
            .db
            .prepare("SELECT name, src FROM tags ORDER BY name")?;
        let mut m: Vec<(String, Vec<String>)> = Vec::new();
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for r in rows {
            let (name, src) = r?;
            match m.last_mut() {
                Some((n, v)) if *n == name => v.push(src),
                _ => m.push((name, vec![src])),
            }
        }
        Ok(m.into_iter()
            .map(|(name, srcs)| json!({"name": name, "srcs": srcs}))
            .collect())
    }

    pub fn notes_with_tag(&self, tag: &str) -> rusqlite::Result<Vec<String>> {
        let t = tag.to_lowercase();
        let mut st = self
            .db
            .prepare("SELECT DISTINCT src FROM tags WHERE name = ?1 OR name LIKE ?2")?;
        let rows = st.query_map(params![t, format!("{t}/%")], |r| r.get(0))?;
        rows.collect()
    }

    /// Full-text search; every word is a prefix query.
    pub fn search(&self, q: &str, limit: usize) -> Vec<Value> {
        let words: Vec<String> = jess_core::names::lookup_key(q)
            .split_whitespace()
            .map(|w| {
                w.chars()
                    .filter(|c| !"\"*^:(){}".contains(*c))
                    .collect::<String>()
            })
            .filter(|w| !w.is_empty())
            .collect();
        if words.is_empty() {
            return vec![];
        }
        let m = words
            .iter()
            .map(|w| format!("\"{w}\"*"))
            .collect::<Vec<_>>()
            .join(" ");
        let run = || -> rusqlite::Result<Vec<Value>> {
            let mut st = self.db.prepare(
                "SELECT id, page, snippet(fts, 3, char(2), char(3), '…', 12) FROM fts WHERE fts MATCH ?1 ORDER BY bm25(fts, 0.0, 0.0, 5.0, 1.0) LIMIT ?2",
            )?;
            let rows = st.query_map(params![m, limit as i64], |r| {
                let page: Option<i64> = r.get(1)?;
                let mut v =
                    json!({"id": r.get::<_, String>(0)?, "snippet": r.get::<_, String>(2)?});
                if let Some(p) = page {
                    v["page"] = json!(p);
                }
                Ok(v)
            })?;
            rows.collect()
        };
        run().unwrap_or_default()
    }
}
