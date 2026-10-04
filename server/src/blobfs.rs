//! Content-addressed blob files (DESIGN §7.1): `blobs/ab/cd/<sha256>`, 0444, written via
//! `blobs/.tmp/` + fsync + rename + dir fsync, so a crash never leaves a partial blob in place.

use jess_core::Hash;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct BlobFs {
    pub root: PathBuf,
    /// fsync files and directories (off only in tests/simulation).
    pub durable: bool,
}

pub fn fsync_dir(p: &Path) -> io::Result<()> {
    File::open(p)?.sync_all()
}

impl BlobFs {
    pub fn new(root: impl Into<PathBuf>, durable: bool) -> io::Result<BlobFs> {
        let root = root.into();
        fs::create_dir_all(root.join(".tmp"))?;
        Ok(BlobFs { root, durable })
    }

    pub fn path(&self, h: &Hash) -> PathBuf {
        let x = h.to_hex();
        self.root.join(&x[0..2]).join(&x[2..4]).join(x)
    }

    fn tmp(&self, upload_id: &str) -> PathBuf {
        let safe: String = upload_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        self.root.join(".tmp").join(safe)
    }

    pub fn exists(&self, h: &Hash) -> bool {
        self.path(h).exists()
    }

    pub fn size(&self, h: &Hash) -> Option<u64> {
        fs::metadata(self.path(h)).ok().map(|m| m.len())
    }

    /// Writes one verified chunk at its offset (pwrite) and fsyncs it.
    pub fn write_chunk(&self, upload_id: &str, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.tmp(upload_id))?;
        f.write_all_at(bytes, offset)?;
        if self.durable {
            f.sync_data()?;
        }
        Ok(())
    }

    /// Streams the temp file through SHA-256; on match moves it into place atomically.
    /// Returns false (and discards the temp file) on mismatch.
    pub fn finalize(&self, upload_id: &str, expected: &Hash, size: u64) -> io::Result<bool> {
        let tmp = self.tmp(upload_id);
        let ok = match File::open(&tmp) {
            Ok(mut f) => {
                let len = f.metadata()?.len();
                len == size && hash_reader(&mut f)? == *expected
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                size == 0 && *expected == Hash::of(b"")
            }
            Err(e) => return Err(e),
        };
        if !ok {
            let _ = fs::remove_file(&tmp);
            return Ok(false);
        }
        if !tmp.exists() {
            File::create(&tmp)?;
        }
        self.install(&tmp, expected)?;
        Ok(true)
    }

    fn install(&self, tmp: &Path, h: &Hash) -> io::Result<()> {
        self.install_with(tmp, h, self.durable)
    }

    fn install_with(&self, tmp: &Path, h: &Hash, durable: bool) -> io::Result<()> {
        let dest = self.path(h);
        let dir = dest.parent().expect("has parent");
        fs::create_dir_all(dir)?;
        if durable {
            File::open(tmp)?.sync_all()?;
        }
        fs::set_permissions(tmp, fs::Permissions::from_mode(0o444))?;
        fs::rename(tmp, &dest)?;
        if durable {
            fsync_dir(dir)?;
        }
        Ok(())
    }

    /// Stores bytes directly (server-side import / tests).
    pub fn put_bytes(&self, bytes: &[u8]) -> io::Result<Hash> {
        let h = Hash::of(bytes);
        if self.exists(&h) {
            return Ok(h);
        }
        let tmp = self.root.join(".tmp").join(format!("put-{}", h.to_hex()));
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        drop(f);
        self.install(&tmp, &h)?;
        Ok(h)
    }

    /// Stores a file by streaming it (hashing on the way).
    pub fn put_reader(&self, r: &mut impl Read, tag: &str) -> io::Result<(Hash, u64)> {
        self.put_reader_with(r, tag, self.durable)
    }

    /// Like `put_reader` but without fsyncs: the caller must call `sync()` before it records the
    /// blob as present (batched server-side import: one filesystem sync per batch instead of a
    /// file and a directory fsync per blob).
    pub fn put_reader_unsynced(&self, r: &mut impl Read, tag: &str) -> io::Result<(Hash, u64)> {
        self.put_reader_with(r, tag, false)
    }

    /// Makes every file written and renamed under the blob root durable (`syncfs`).
    pub fn sync(&self) -> io::Result<()> {
        if !self.durable {
            return Ok(());
        }
        use std::os::fd::AsRawFd;
        let d = File::open(&self.root)?;
        // SAFETY: a valid open descriptor for the duration of the call.
        if unsafe { libc::syncfs(d.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn put_reader_with(
        &self,
        r: &mut impl Read,
        tag: &str,
        durable: bool,
    ) -> io::Result<(Hash, u64)> {
        let tmp = self.root.join(".tmp").join(format!("in-{tag}"));
        let mut f = File::create(&tmp)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut n = 0u64;
        loop {
            let k = r.read(&mut buf)?;
            if k == 0 {
                break;
            }
            hasher.update(&buf[..k]);
            f.write_all(&buf[..k])?;
            n += k as u64;
        }
        drop(f);
        let h = Hash(hasher.finalize().into());
        if self.exists(&h) {
            fs::remove_file(&tmp)?;
        } else {
            self.install_with(&tmp, &h, durable)?;
        }
        Ok((h, n))
    }

    pub fn read(&self, h: &Hash) -> io::Result<Vec<u8>> {
        fs::read(self.path(h))
    }

    pub fn read_range(&self, h: &Hash, offset: u64, len: u64) -> io::Result<Vec<u8>> {
        let f = File::open(self.path(h))?;
        let size = f.metadata()?.len();
        let end = (offset + len).min(size);
        let mut buf = vec![0u8; end.saturating_sub(offset) as usize];
        f.read_exact_at(&mut buf, offset)?;
        Ok(buf)
    }

    pub fn delete(&self, h: &Hash) -> io::Result<()> {
        match fs::remove_file(self.path(h)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    pub fn discard_tmp(&self, upload_id: &str) {
        let _ = fs::remove_file(self.tmp(upload_id));
    }

    /// Re-hashes a stored blob.
    pub fn verify(&self, h: &Hash) -> io::Result<bool> {
        let mut f = File::open(self.path(h))?;
        Ok(hash_reader(&mut f)? == *h)
    }

    pub fn tmp_files(&self) -> Vec<PathBuf> {
        fs::read_dir(self.root.join(".tmp"))
            .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default()
    }
}

pub fn hash_reader(r: &mut impl Read) -> io::Result<Hash> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let k = r.read(&mut buf)?;
        if k == 0 {
            break;
        }
        hasher.update(&buf[..k]);
    }
    Ok(Hash(hasher.finalize().into()))
}
