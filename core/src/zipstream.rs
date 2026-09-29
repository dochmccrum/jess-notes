//! A streaming ZIP writer (no `Seek`): local headers with data descriptors, ZIP64 when needed,
//! UTF-8 names, stored or deflated entries. Lets a multi-GB vault export stream straight to a
//! socket or a file picker without temp files or holding anything in memory.

use flate2::write::DeflateEncoder;
use flate2::Compression;
use std::io::{self, Read, Write};

const U32_MAX: u64 = 0xFFFF_FFFF;

struct Counting<W> {
    w: W,
    n: u64,
}

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let k = self.w.write(b)?;
        self.n += k as u64;
        Ok(k)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }
}

struct Cd {
    name: Vec<u8>,
    method: u16,
    time: u16,
    date: u16,
    crc: u32,
    csize: u64,
    usize: u64,
    offset: u64,
    dir: bool,
    mtime_s: u32,
}

pub struct ZipStream<W: Write> {
    w: Counting<W>,
    cd: Vec<Cd>,
}

/// DOS date/time (UTC) from unix milliseconds.
pub fn dos_datetime(ms: u64) -> (u16, u16) {
    let secs = ms / 1000;
    let days = secs / 86400;
    let rem = secs % 86400;
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    if y < 1980 {
        return (0, (1 << 5) | 1);
    }
    let time = (((rem / 3600) << 11) | (((rem % 3600) / 60) << 5) | ((rem % 60) / 2)) as u16;
    let date = (((y - 1980) as u16) << 9) | ((m as u16) << 5) | d as u16;
    (time, date)
}

struct CrcReader<'a, R: Read + ?Sized> {
    r: &'a mut R,
    crc: crc32fast::Hasher,
    n: u64,
}

impl<R: Read + ?Sized> Read for CrcReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let k = self.r.read(buf)?;
        self.crc.update(&buf[..k]);
        self.n += k as u64;
        Ok(k)
    }
}

impl<W: Write> ZipStream<W> {
    pub fn new(w: W) -> ZipStream<W> {
        ZipStream {
            w: Counting { w, n: 0 },
            cd: Vec::new(),
        }
    }

    fn local_header(
        &mut self,
        name: &[u8],
        method: u16,
        time: u16,
        date: u16,
        z64: bool,
        mtime_s: u32,
    ) -> io::Result<()> {
        let mut h = Vec::with_capacity(64 + name.len());
        h.extend_from_slice(&0x04034b50u32.to_le_bytes());
        h.extend_from_slice(&(if z64 { 45u16 } else { 20 }).to_le_bytes());
        h.extend_from_slice(&((1u16 << 3) | (1 << 11)).to_le_bytes()); // data descriptor + UTF-8
        h.extend_from_slice(&method.to_le_bytes());
        h.extend_from_slice(&time.to_le_bytes());
        h.extend_from_slice(&date.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes()); // crc (in descriptor)
        let sz = if z64 { 0xFFFF_FFFFu32 } else { 0 };
        h.extend_from_slice(&sz.to_le_bytes());
        h.extend_from_slice(&sz.to_le_bytes());
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        let extra_len: u16 = 9 + if z64 { 20 } else { 0 };
        h.extend_from_slice(&extra_len.to_le_bytes());
        h.extend_from_slice(name);
        // Extended timestamp (0x5455): exact mtime seconds.
        h.extend_from_slice(&0x5455u16.to_le_bytes());
        h.extend_from_slice(&5u16.to_le_bytes());
        h.push(1);
        h.extend_from_slice(&mtime_s.to_le_bytes());
        if z64 {
            h.extend_from_slice(&0x0001u16.to_le_bytes());
            h.extend_from_slice(&16u16.to_le_bytes());
            h.extend_from_slice(&0u64.to_le_bytes());
            h.extend_from_slice(&0u64.to_le_bytes());
        }
        self.w.write_all(&h)
    }

    pub fn add_dir(&mut self, path: &str, mtime_ms: Option<u64>) -> io::Result<()> {
        let mut name = path.trim_end_matches('/').as_bytes().to_vec();
        name.push(b'/');
        let (time, date) = dos_datetime(mtime_ms.unwrap_or(0));
        let offset = self.w.n;
        let mtime_s = (mtime_ms.unwrap_or(0) / 1000) as u32;
        let z64 = offset >= U32_MAX;
        self.local_header(&name, 0, time, date, z64, mtime_s)?;
        self.descriptor(0, 0, 0, z64)?;
        self.cd.push(Cd {
            name,
            method: 0,
            time,
            date,
            crc: 0,
            csize: 0,
            usize: 0,
            offset,
            dir: true,
            mtime_s,
        });
        Ok(())
    }

    fn descriptor(&mut self, crc: u32, csize: u64, usize: u64, z64: bool) -> io::Result<()> {
        let mut d = Vec::with_capacity(24);
        d.extend_from_slice(&0x08074b50u32.to_le_bytes());
        d.extend_from_slice(&crc.to_le_bytes());
        if z64 {
            d.extend_from_slice(&csize.to_le_bytes());
            d.extend_from_slice(&usize.to_le_bytes());
        } else {
            d.extend_from_slice(&(csize as u32).to_le_bytes());
            d.extend_from_slice(&(usize as u32).to_le_bytes());
        }
        self.w.write_all(&d)
    }

    /// Streams one file. `size_hint` decides ZIP64 up front (it must be ≥ the real size when
    /// the real size is ≥ 4 GiB).
    pub fn add_file(
        &mut self,
        path: &str,
        size_hint: u64,
        mtime_ms: Option<u64>,
        deflate: bool,
        r: &mut dyn Read,
    ) -> io::Result<()> {
        let name = path.as_bytes().to_vec();
        let (time, date) = dos_datetime(mtime_ms.unwrap_or(0));
        let mtime_s = (mtime_ms.unwrap_or(0) / 1000) as u32;
        let offset = self.w.n;
        let z64 = size_hint >= U32_MAX - (1 << 20) || offset >= U32_MAX;
        let method = if deflate { 8 } else { 0 };
        self.local_header(&name, method, time, date, z64, mtime_s)?;
        let start = self.w.n;
        let mut cr = CrcReader {
            r,
            crc: crc32fast::Hasher::new(),
            n: 0,
        };
        if deflate {
            let mut enc = DeflateEncoder::new(&mut self.w, Compression::default());
            io::copy(&mut cr, &mut enc)?;
            enc.finish()?;
        } else {
            io::copy(&mut cr, &mut self.w)?;
        }
        let csize = self.w.n - start;
        let usize = cr.n;
        if !z64 && (usize >= U32_MAX || csize >= U32_MAX) {
            return Err(io::Error::other(
                "entry exceeded its size hint; ZIP64 was not enabled",
            ));
        }
        let crc = cr.crc.finalize();
        self.descriptor(crc, csize, usize, z64)?;
        self.cd.push(Cd {
            name,
            method,
            time,
            date,
            crc,
            csize,
            usize,
            offset,
            dir: false,
            mtime_s,
        });
        Ok(())
    }

    pub fn add_bytes(
        &mut self,
        path: &str,
        bytes: &[u8],
        mtime_ms: Option<u64>,
        deflate: bool,
    ) -> io::Result<()> {
        self.add_file(path, bytes.len() as u64, mtime_ms, deflate, &mut &bytes[..])
    }

    /// Writes the central directory and returns the inner writer.
    pub fn finish(mut self) -> io::Result<W> {
        let cd_start = self.w.n;
        let cd = std::mem::take(&mut self.cd);
        for e in &cd {
            let need64 = e.csize >= U32_MAX || e.usize >= U32_MAX || e.offset >= U32_MAX;
            let mut h = Vec::with_capacity(80 + e.name.len());
            h.extend_from_slice(&0x02014b50u32.to_le_bytes());
            h.extend_from_slice(&((3u16 << 8) | 45).to_le_bytes()); // made by: unix, 4.5
            h.extend_from_slice(&(if need64 { 45u16 } else { 20 }).to_le_bytes());
            h.extend_from_slice(&((1u16 << 3) | (1 << 11)).to_le_bytes());
            h.extend_from_slice(&e.method.to_le_bytes());
            h.extend_from_slice(&e.time.to_le_bytes());
            h.extend_from_slice(&e.date.to_le_bytes());
            h.extend_from_slice(&e.crc.to_le_bytes());
            let (cs, us, off) = if need64 {
                (0xFFFF_FFFFu32, 0xFFFF_FFFFu32, 0xFFFF_FFFFu32)
            } else {
                (e.csize as u32, e.usize as u32, e.offset as u32)
            };
            h.extend_from_slice(&cs.to_le_bytes());
            h.extend_from_slice(&us.to_le_bytes());
            h.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            let extra_len: u16 = 9 + if need64 { 28 } else { 0 };
            h.extend_from_slice(&extra_len.to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes()); // comment
            h.extend_from_slice(&0u16.to_le_bytes()); // disk
            h.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            let mode: u32 = if e.dir { 0o40755 } else { 0o100644 };
            h.extend_from_slice(&((mode << 16) | if e.dir { 0x10 } else { 0 }).to_le_bytes());
            h.extend_from_slice(&off.to_le_bytes());
            h.extend_from_slice(&e.name);
            h.extend_from_slice(&0x5455u16.to_le_bytes());
            h.extend_from_slice(&5u16.to_le_bytes());
            h.push(1);
            h.extend_from_slice(&e.mtime_s.to_le_bytes());
            if need64 {
                h.extend_from_slice(&0x0001u16.to_le_bytes());
                h.extend_from_slice(&24u16.to_le_bytes());
                h.extend_from_slice(&e.usize.to_le_bytes());
                h.extend_from_slice(&e.csize.to_le_bytes());
                h.extend_from_slice(&e.offset.to_le_bytes());
            }
            self.w.write_all(&h)?;
        }
        let cd_end = self.w.n;
        let cd_size = cd_end - cd_start;
        let count = cd.len() as u64;
        let z64 = count >= 0xFFFF || cd_size >= U32_MAX || cd_start >= U32_MAX;
        if z64 {
            let mut r = Vec::with_capacity(76);
            r.extend_from_slice(&0x06064b50u32.to_le_bytes());
            r.extend_from_slice(&44u64.to_le_bytes());
            r.extend_from_slice(&((3u16 << 8) | 45).to_le_bytes());
            r.extend_from_slice(&45u16.to_le_bytes());
            r.extend_from_slice(&0u32.to_le_bytes());
            r.extend_from_slice(&0u32.to_le_bytes());
            r.extend_from_slice(&count.to_le_bytes());
            r.extend_from_slice(&count.to_le_bytes());
            r.extend_from_slice(&cd_size.to_le_bytes());
            r.extend_from_slice(&cd_start.to_le_bytes());
            // locator
            r.extend_from_slice(&0x07064b50u32.to_le_bytes());
            r.extend_from_slice(&0u32.to_le_bytes());
            r.extend_from_slice(&cd_end.to_le_bytes());
            r.extend_from_slice(&1u32.to_le_bytes());
            self.w.write_all(&r)?;
        }
        let mut e = Vec::with_capacity(22);
        e.extend_from_slice(&0x06054b50u32.to_le_bytes());
        e.extend_from_slice(&0u16.to_le_bytes());
        e.extend_from_slice(&0u16.to_le_bytes());
        let c16 = if z64 { 0xFFFFu16 } else { count as u16 };
        e.extend_from_slice(&c16.to_le_bytes());
        e.extend_from_slice(&c16.to_le_bytes());
        e.extend_from_slice(&(if z64 { 0xFFFF_FFFFu32 } else { cd_size as u32 }).to_le_bytes());
        e.extend_from_slice(&(if z64 { 0xFFFF_FFFFu32 } else { cd_start as u32 }).to_le_bytes());
        e.extend_from_slice(&0u16.to_le_bytes());
        self.w.write_all(&e)?;
        self.w.flush()?;
        Ok(self.w.w)
    }
}

#[cfg(all(test, feature = "zip"))]
mod tests {
    use super::*;
    #[test]
    fn readable_by_zip_crate() {
        let mut z = ZipStream::new(Vec::new());
        z.add_dir("empty dir", Some(1_700_000_000_000)).unwrap();
        z.add_bytes(
            "Notes/Ünï.md",
            "héllo\r\n".repeat(1000).as_bytes(),
            Some(1_700_000_000_000),
            true,
        )
        .unwrap();
        z.add_bytes("img.png", &[0u8, 1, 2, 3], None, false)
            .unwrap();
        let bytes = z.finish().unwrap();
        let mut a = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        assert_eq!(a.len(), 3);
        let mut s = String::new();
        a.by_name("Notes/Ünï.md")
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        assert_eq!(s, "héllo\r\n".repeat(1000));
        let mut v = Vec::new();
        a.by_name("img.png").unwrap().read_to_end(&mut v).unwrap();
        assert_eq!(v, vec![0, 1, 2, 3]);
        assert!(a.by_name("empty dir/").unwrap().is_dir());
    }
    #[test]
    fn dos_time() {
        assert_eq!(dos_datetime(0), (0, 33));
        let (t, d) = dos_datetime(1_759_150_800_000); // 2025-09-29 13:00:00 UTC
        assert_eq!(d, ((2025 - 1980) << 9) | (9 << 5) | 29);
        assert_eq!(t, 13 << 11);
    }
}
