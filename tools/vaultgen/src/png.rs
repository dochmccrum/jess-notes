//! A minimal PNG writer: 8-bit RGB, one IDAT, fast deflate. Gradients compress to almost
//! nothing; `noise` (0–255) adds per-pixel noise to a band of rows (an eighth of the image), so
//! file sizes vary like screenshots and photos do without making the vault huge.

use crate::Rng;
use flate2::{write::ZlibEncoder, Compression, Crc};
use std::io::Write;

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut c = Crc::new();
    c.update(kind);
    c.update(data);
    out.extend_from_slice(&c.sum().to_be_bytes());
}

pub fn image(r: &mut Rng, w: usize, h: usize, noise: u8) -> Vec<u8> {
    let base = [r.below(256) as u8, r.below(256) as u8, r.below(256) as u8];
    let mut raw = Vec::with_capacity((w * 3 + 1) * h);
    let mut s = r.next_u64() | 1;
    let band = r.below(h.max(8) * 7 / 8 + 1);
    for y in 0..h {
        let noisy = noise > 0 && y >= band && y < band + h / 8;
        raw.push(1); // filter: Sub (each byte minus the one a pixel to the left)
        let mut left = [0u8; 3];
        for x in 0..w {
            for (c, b) in base.iter().enumerate() {
                let mut v = b.wrapping_add(((x * (c + 1) + y * (3 - c)) / 4) as u8);
                if noisy {
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    v = v.wrapping_add((s % noise as u64) as u8);
                }
                raw.push(v.wrapping_sub(left[c]));
                left[c] = v;
            }
        }
    }
    let mut z = ZlibEncoder::new(Vec::new(), Compression::fast());
    z.write_all(&raw).unwrap();
    let idat = z.finish().unwrap();
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, RGB, deflate, adaptive filtering, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    out
}
