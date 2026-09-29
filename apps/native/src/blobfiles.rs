//! Local blob bytes as files (DESIGN §7.5): `<dir>/ab/cd/<hash>`. Partially downloaded blobs are
//! sparse files written chunk by chunk; which chunks are present is tracked by the core's blob
//! state machine, never inferred from the file.

use jess_core::Hash;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct BlobFiles {
    root: PathBuf,
}

impl BlobFiles {
    pub fn new(root: impl Into<PathBuf>) -> io::Result<BlobFiles> {
        let root = root.into();
        fs::create_dir_all(root.join(".tmp"))?;
        Ok(BlobFiles { root })
    }

    pub fn path(&self, h: &Hash) -> PathBuf {
        let x = h.to_hex();
        self.root.join(&x[0..2]).join(&x[2..4]).join(x)
    }

    /// Writes one downloaded chunk at its offset (sparse file).
    pub fn write_at(&self, h: &Hash, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let p = self.path(h);
        fs::create_dir_all(p.parent().expect("parent"))?;
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&p)?;
        f.seek(SeekFrom::Start(offset))?;
        f.write_all(bytes)?;
        f.sync_data()
    }

    pub fn read_at(&self, h: &Hash, offset: u64, len: u64) -> io::Result<Vec<u8>> {
        let mut f = File::open(self.path(h))?;
        f.seek(SeekFrom::Start(offset))?;
        let mut v = vec![0; len as usize];
        f.read_exact(&mut v)?;
        Ok(v)
    }

    pub fn delete(&self, h: &Hash) -> io::Result<()> {
        match fs::remove_file(self.path(h)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Streams `r` into the store while hashing; returns `(hash, size, first 64 KiB)`.
    /// The file is fsynced and renamed into place, so a crash never leaves a partial blob.
    pub fn ingest(&self, r: &mut dyn Read) -> io::Result<(Hash, u64, Vec<u8>)> {
        let tmp = self
            .root
            .join(".tmp")
            .join(format!("ingest-{:016x}", rand::random::<u64>()));
        let res = (|| {
            let mut f = File::create(&tmp)?;
            let mut hasher = Sha256::new();
            let mut header = Vec::new();
            let mut size = 0u64;
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = r.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                if header.len() < 65536 {
                    header.extend_from_slice(&buf[..n.min(65536 - header.len())]);
                }
                hasher.update(&buf[..n]);
                f.write_all(&buf[..n])?;
                size += n as u64;
            }
            f.sync_all()?;
            let h = Hash(hasher.finalize().into());
            let dst = self.path(&h);
            fs::create_dir_all(dst.parent().expect("parent"))?;
            fs::rename(&tmp, &dst)?;
            Ok((h, size, header))
        })();
        if res.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        res
    }

    pub fn ingest_path(&self, p: &Path) -> io::Result<(Hash, u64, Vec<u8>)> {
        self.ingest(&mut File::open(p)?)
    }
}
