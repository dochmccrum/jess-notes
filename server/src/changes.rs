//! Reading `Changes` pages (DESIGN §5.3): everything in `(from, to]`, cut at commit boundaries.

use crate::db::{blob_from_row, entry_from_row, BLOB_COLS, ENTRY_COLS};
use crate::error::Result;
use jess_core::hlc::Hlc;
use jess_core::proto::{Changes, DocUpdate};
use jess_core::Id;
use rusqlite::{params, Connection, OptionalExtension};

/// Reads the next page after `from` (up to `head`). Own updates (origin == `replica`) become
/// `Own` placeholders.
pub fn read_changes(
    conn: &Connection,
    from: u64,
    head: u64,
    replica: u64,
    limit_bytes: u64,
    hlc: Hlc,
) -> Result<Changes> {
    if from >= head {
        return Ok(Changes {
            from,
            to: from.max(head),
            hlc,
            ..Default::default()
        });
    }
    // Find the cut point from doc sizes.
    let mut to = head;
    {
        let mut st = conn.prepare_cached("SELECT seq, length(\"update\"), origin_replica FROM doc_updates WHERE seq > ?1 AND seq <= ?2 ORDER BY seq")?;
        let mut rows = st.query(params![from as i64, head as i64])?;
        let mut cum: u64 = 0;
        let mut over: Option<u64> = None;
        while let Some(r) = rows.next()? {
            let seq: i64 = r.get(0)?;
            let len: i64 = r.get(1)?;
            let origin: Option<i64> = r.get(2)?;
            if origin == Some(replica as i64) {
                continue;
            }
            cum += len as u64;
            if cum > limit_bytes {
                over = Some(seq as u64);
                break;
            }
        }
        if let Some(s) = over {
            let prev_end: Option<i64> = conn
                .query_row(
                    "SELECT max(last_seq) FROM commits WHERE last_seq < ?1 AND last_seq > ?2",
                    params![s as i64, from as i64],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            to = match prev_end {
                Some(e) => e as u64,
                None => conn
                    .query_row(
                        "SELECT min(last_seq) FROM commits WHERE last_seq >= ?1",
                        params![s as i64],
                        |r| r.get::<_, Option<i64>>(0),
                    )?
                    .map(|v| v as u64)
                    .unwrap_or(head),
            }
            .min(head);
        }
    }
    let mut c = Changes {
        from,
        to,
        hlc,
        more: to < head,
        ..Default::default()
    };
    {
        let mut st = conn.prepare_cached(&format!(
            "SELECT {ENTRY_COLS} FROM entries WHERE seq > ?1 AND seq <= ?2 ORDER BY seq"
        ))?;
        c.entries = st
            .query_map(params![from as i64, to as i64], entry_from_row)?
            .collect::<std::result::Result<_, _>>()?;
    }
    {
        let mut st = conn.prepare_cached(&format!(
            "SELECT {BLOB_COLS} FROM blobs WHERE seq > ?1 AND seq <= ?2 ORDER BY seq"
        ))?;
        c.blobs = st
            .query_map(params![from as i64, to as i64], blob_from_row)?
            .collect::<std::result::Result<_, _>>()?;
    }
    {
        let mut st = conn.prepare_cached("SELECT seq, entry_id, slot, CASE WHEN origin_replica = ?3 THEN NULL ELSE \"update\" END FROM doc_updates WHERE seq > ?1 AND seq <= ?2 ORDER BY seq")?;
        c.docs = st
            .query_map(params![from as i64, to as i64, replica as i64], |r| {
                let e: Vec<u8> = r.get(1)?;
                Ok(DocUpdate {
                    seq: r.get::<_, i64>(0)? as u64,
                    entry: Id::from_slice(&e).unwrap_or_default(),
                    slot: r.get(2)?,
                    bytes: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
    }
    Ok(c)
}
