//! SQLite schema (DESIGN §4.1), migrations, pragmas and row codecs.

use jess_core::hlc::Hlc;
use jess_core::model::{BlobRow, Clocks, Entry, PropValue, Trashed};
use jess_core::{Hash, Id};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::BTreeMap;

pub const SCHEMA_VERSION: i64 = 1;

const MIGRATION_1: &str = r#"
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value BLOB NOT NULL);
CREATE TABLE entries(
  id BLOB PRIMARY KEY, kind TEXT NOT NULL,
  parent_id BLOB, name TEXT NOT NULL, name_key TEXT NOT NULL,
  trashed_batch BLOB, trashed_at INTEGER, tree_visible INTEGER NOT NULL DEFAULT 1,
  blob BLOB, created_at INTEGER, modified_at INTEGER,
  props BLOB, clock BLOB NOT NULL, purged INTEGER NOT NULL DEFAULT 0, purged_at INTEGER,
  seq INTEGER NOT NULL);
CREATE UNIQUE INDEX entries_live_name ON entries(ifnull(parent_id, x''), name_key)
  WHERE trashed_batch IS NULL AND purged = 0 AND kind != 'vault';
CREATE INDEX entries_seq ON entries(seq);
CREATE INDEX entries_parent ON entries(parent_id);
CREATE INDEX entries_blob ON entries(blob);

CREATE TABLE doc_updates(
  seq INTEGER PRIMARY KEY, entry_id BLOB NOT NULL, slot TEXT NOT NULL,
  "update" BLOB NOT NULL, origin_replica INTEGER, merged INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL DEFAULT 0);
CREATE INDEX doc_updates_doc ON doc_updates(entry_id, slot, seq);
CREATE TABLE doc_purged(entry_id BLOB, slot TEXT, state_vector BLOB, state BLOB,
  PRIMARY KEY(entry_id, slot));

CREATE TABLE commits(last_seq INTEGER PRIMARY KEY, first_seq INTEGER NOT NULL);

CREATE TABLE replicas(replica_id INTEGER PRIMARY KEY, device_id BLOB, last_op_id INTEGER NOT NULL,
  last_seen INTEGER, pruned_upto INTEGER NOT NULL DEFAULT 0);
CREATE TABLE op_results(replica_id INTEGER, op_id INTEGER, seq INTEGER, result BLOB, at INTEGER,
  PRIMARY KEY(replica_id, op_id));
CREATE TABLE rename_history(seq INTEGER, entry_id BLOB, old_path TEXT, new_path TEXT,
  is_folder INTEGER NOT NULL, origin_replica INTEGER, origin_op INTEGER, induced INTEGER NOT NULL);
CREATE INDEX rename_history_seq ON rename_history(seq);
CREATE TABLE links(src BLOB, slot TEXT, ord INTEGER, start16 INTEGER, end16 INTEGER,
  tstart16 INTEGER, tend16 INTEGER, syntax INTEGER, embed INTEGER, target TEXT, raw_target TEXT,
  angle INTEGER, target_key TEXT, subpath TEXT, display TEXT, resolved BLOB,
  PRIMARY KEY(src, slot, ord));
CREATE INDEX links_key ON links(target_key);
CREATE INDEX links_resolved ON links(resolved);

CREATE TABLE blobs(hash BLOB PRIMARY KEY, size INTEGER, mime TEXT, width INTEGER, height INTEGER,
  orientation INTEGER, present INTEGER NOT NULL, stored_at INTEGER,
  unreferenced_since INTEGER, seq INTEGER NOT NULL);
CREATE INDEX blobs_seq ON blobs(seq);
CREATE TABLE uploads(upload_id TEXT PRIMARY KEY, hash BLOB, size INTEGER, chunk_size INTEGER,
  received BLOB, created_at INTEGER, touched_at INTEGER, device_id BLOB);
CREATE TABLE derived(hash BLOB, kind TEXT, status TEXT, version INTEGER, size INTEGER, error TEXT,
  PRIMARY KEY(hash, kind));

CREATE TABLE account(id INTEGER PRIMARY KEY CHECK(id=1), password_hash TEXT, created_at INTEGER,
  from_env INTEGER NOT NULL DEFAULT 0);
CREATE TABLE devices(id BLOB PRIMARY KEY, name TEXT, token_hash BLOB UNIQUE, created_at INTEGER,
  last_seen INTEGER, revoked_at INTEGER);
CREATE TABLE pairing_codes(code_hash BLOB PRIMARY KEY, expires_at INTEGER, used_at INTEGER);
"#;

/// Opens (and migrates) the database. `durable = false` relaxes fsync for tests/simulation.
pub fn open(path: &std::path::Path, durable: bool) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    configure(&conn, durable)?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn open_memory() -> rusqlite::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn, false)?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn open_readonly(path: &std::path::Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

fn configure(conn: &Connection, durable: bool) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", if durable { "FULL" } else { "OFF" })?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(())
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value BLOB NOT NULL);",
    )?;
    let v: i64 = get_meta_i64(conn, "schema_version")?.unwrap_or(0);
    if v < 1 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_1)?;
        set_meta_i64(&tx, "schema_version", 1)?;
        tx.commit()?;
    }
    Ok(())
}

pub fn get_meta(conn: &Connection, key: &str) -> rusqlite::Result<Option<Vec<u8>>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()
}
pub fn set_meta(conn: &Connection, key: &str, v: &[u8]) -> rusqlite::Result<()> {
    conn.execute("INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, v])?;
    Ok(())
}
pub fn get_meta_i64(conn: &Connection, key: &str) -> rusqlite::Result<Option<i64>> {
    Ok(get_meta(conn, key)?
        .and_then(|v| v.try_into().ok())
        .map(i64::from_be_bytes))
}
pub fn set_meta_i64(conn: &Connection, key: &str, v: i64) -> rusqlite::Result<()> {
    set_meta(conn, key, &v.to_be_bytes())
}

fn blob_opt<T>(v: Option<Vec<u8>>, f: impl Fn(&[u8]) -> Option<T>) -> Option<T> {
    v.and_then(|b| f(&b))
}

pub const ENTRY_COLS: &str = "id, kind, parent_id, name, trashed_batch, trashed_at, tree_visible, blob, created_at, modified_at, props, clock, purged, seq";

pub fn entry_from_row(r: &Row) -> rusqlite::Result<Entry> {
    let id: Vec<u8> = r.get(0)?;
    let tb: Option<Vec<u8>> = r.get(4)?;
    let ta: Option<i64> = r.get(5)?;
    let props: Option<Vec<u8>> = r.get(10)?;
    let clock: Vec<u8> = r.get(11)?;
    Ok(Entry {
        id: Id::from_slice(&id).unwrap_or_default(),
        kind: r.get(1)?,
        parent: blob_opt(r.get(2)?, Id::from_slice),
        name: r.get(3)?,
        trashed: blob_opt(tb, Id::from_slice).map(|batch| Trashed {
            batch,
            at: ta.unwrap_or(0) as u64,
        }),
        tree_visible: r.get::<_, i64>(6)? != 0,
        blob: blob_opt(r.get(7)?, Hash::from_slice),
        created_at: r.get::<_, Option<i64>>(8)?.map(|v| v as u64),
        modified_at: r.get::<_, Option<i64>>(9)?.map(|v| v as u64),
        props: props
            .and_then(|p| minicbor::decode::<BTreeMap<String, PropValue>>(&p).ok())
            .unwrap_or_default(),
        purged: r.get::<_, i64>(12)? != 0,
        clock: minicbor::decode::<Clocks>(&clock).unwrap_or_default(),
        seq: r.get::<_, i64>(13)? as u64,
    })
}

pub fn put_entry(conn: &Connection, e: &Entry, purged_at: Option<u64>) -> rusqlite::Result<()> {
    let props = if e.props.is_empty() {
        None
    } else {
        Some(minicbor::to_vec(&e.props).expect("encode"))
    };
    conn.execute(
        "INSERT INTO entries(id, kind, parent_id, name, name_key, trashed_batch, trashed_at, tree_visible, blob, created_at, modified_at, props, clock, purged, purged_at, seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, parent_id=excluded.parent_id, name=excluded.name, name_key=excluded.name_key,
           trashed_batch=excluded.trashed_batch, trashed_at=excluded.trashed_at, tree_visible=excluded.tree_visible, blob=excluded.blob,
           created_at=excluded.created_at, modified_at=excluded.modified_at, props=excluded.props, clock=excluded.clock,
           purged=excluded.purged, purged_at=coalesce(excluded.purged_at, entries.purged_at), seq=excluded.seq",
        params![
            e.id.0.to_vec(),
            e.kind,
            e.parent.map(|p| p.0.to_vec()),
            e.name,
            jess_core::names::name_key(&e.name),
            e.trashed.map(|t| t.batch.0.to_vec()),
            e.trashed.map(|t| t.at as i64),
            e.tree_visible as i64,
            e.blob.map(|h| h.0.to_vec()),
            e.created_at.map(|v| v as i64),
            e.modified_at.map(|v| v as i64),
            props,
            minicbor::to_vec(&e.clock).expect("encode"),
            e.purged as i64,
            purged_at.map(|v| v as i64),
            e.seq as i64
        ],
    )?;
    Ok(())
}

pub const BLOB_COLS: &str = "hash, size, mime, width, height, orientation, present, seq";

pub fn blob_from_row(r: &Row) -> rusqlite::Result<BlobRow> {
    let h: Vec<u8> = r.get(0)?;
    Ok(BlobRow {
        hash: Hash::from_slice(&h).unwrap_or_default(),
        size: r.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
        mime: r.get(2)?,
        width: r.get::<_, Option<i64>>(3)?.map(|v| v as u32),
        height: r.get::<_, Option<i64>>(4)?.map(|v| v as u32),
        orientation: r.get::<_, Option<i64>>(5)?.map(|v| v as u8),
        present: r.get::<_, i64>(6)? != 0,
        seq: r.get::<_, i64>(7)? as u64,
    })
}

pub fn load_entries(conn: &Connection) -> rusqlite::Result<Vec<Entry>> {
    let mut st = conn.prepare(&format!("SELECT {ENTRY_COLS} FROM entries"))?;
    let v = st
        .query_map([], entry_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

pub fn hlc_to_bytes(h: Hlc) -> Vec<u8> {
    minicbor::to_vec(h).expect("encode")
}
