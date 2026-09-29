//! Snapshots (DESIGN §15): SQLite online backup → `snapshots/jess-<UTC>.db.zst`, with retention:
//! everything from the last 48 h, then one per day up to the retention limit.

use crate::error::{Error, Result};
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};

pub fn utc_stamp(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs / 86400, secs % 86400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // civil-from-days (Howard Hinnant)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}{mo:02}{d:02}T{h:02}{m:02}{s:02}Z")
}

/// Writes a compressed snapshot of the database at `db` into `dir`. Returns its path.
pub fn snapshot(db: &Path, dir: &Path, now: u64) -> Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".tmp-{}.db", utc_stamp(now)));
    {
        let src = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut dst = Connection::open(&tmp)?;
        let b = rusqlite::backup::Backup::new(&src, &mut dst)?;
        b.run_to_completion(256, std::time::Duration::from_millis(5), None)?;
    }
    let out = dir.join(format!("jess-{}.db.zst", utc_stamp(now)));
    let part = out.with_extension("zst.part");
    {
        let mut input = fs::File::open(&tmp)?;
        let f = fs::File::create(&part)?;
        let mut enc = zstd::Encoder::new(f, 9)?;
        std::io::copy(&mut input, &mut enc)?;
        let f = enc.finish()?;
        f.sync_all()?;
    }
    fs::rename(&part, &out)?;
    fs::remove_file(&tmp)?;
    let _ = crate::blobfs::fsync_dir(dir);
    Ok(out)
}

pub fn list(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("jess-") && n.ends_with(".db.zst"))
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Applies retention; returns deleted files. `stamps` come from file names.
pub fn prune(dir: &Path, now: u64, retention_days: u64) -> Result<Vec<PathBuf>> {
    let mut deleted = Vec::new();
    let mut kept_days = std::collections::HashSet::new();
    let recent = utc_stamp(now.saturating_sub(48 * 3_600_000));
    let oldest = utc_stamp(now.saturating_sub(retention_days * 86_400_000));
    for p in list(dir).into_iter().rev() {
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let stamp = name
            .trim_start_matches("jess-")
            .trim_end_matches(".db.zst")
            .to_string();
        let keep = if stamp >= recent {
            true
        } else if stamp < oldest {
            false
        } else {
            kept_days.insert(stamp[..8].to_string())
        };
        if !keep {
            fs::remove_file(&p).map_err(Error::from)?;
            deleted.push(p);
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stamps() {
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        assert_eq!(utc_stamp(1_759_150_800_000), "20250929T130000Z");
    }
}
