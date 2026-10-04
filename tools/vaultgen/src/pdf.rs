//! A minimal PDF writer: `pages` pages of Helvetica text, padded to about `bytes` with one
//! uncompressed image per page (so the bytes are spread over the file like a scanned document,
//! and a viewer that fetches by range needs only a small part for the first page).

use crate::text::Words;
use crate::Rng;

pub fn make(r: &mut Rng, w: &Words, pages: usize, bytes: usize) -> Vec<u8> {
    let pages = pages.max(1);
    // Objects: 1 catalog, 2 pages, 3 font, then per page: page, contents, image.
    let per_page_img = bytes.saturating_sub(pages * 1500) / pages;
    let (iw, ih) = {
        let px = (per_page_img / 3).max(1);
        let iw = (px as f64).sqrt().ceil().max(1.0) as usize;
        (iw, (px / iw).max(1))
    };
    let mut out = Vec::with_capacity(bytes + 4096);
    let mut offsets = vec![0usize; 3 + pages * 3 + 1];
    out.extend_from_slice(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n");
    let obj = |out: &mut Vec<u8>, offsets: &mut Vec<usize>, id: usize, body: &[u8]| {
        offsets[id] = out.len();
        out.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    obj(
        &mut out,
        &mut offsets,
        1,
        b"<< /Type /Catalog /Pages 2 0 R >>",
    );
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 4 + i * 3)).collect();
    obj(
        &mut out,
        &mut offsets,
        2,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            kids.join(" ")
        )
        .as_bytes(),
    );
    obj(
        &mut out,
        &mut offsets,
        3,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    );
    let mut s = r.next_u64() | 1;
    for i in 0..pages {
        let (p, c, im) = (4 + i * 3, 5 + i * 3, 6 + i * 3);
        obj(
            &mut out,
            &mut offsets,
            p,
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> /XObject << /Im1 {im} 0 R >> >> /Contents {c} 0 R >>").as_bytes(),
        );
        let mut text = format!(
            "BT /F1 20 Tf 72 740 Td (Page {}) Tj ET\nBT /F1 11 Tf 72 712 Td 14 TL\n",
            i + 1
        );
        for _ in 0..20 {
            let line: String = w
                .sentence(r)
                .chars()
                .filter(|c| c.is_ascii() && !matches!(c, '(' | ')' | '\\'))
                .take(90)
                .collect();
            text.push_str(&format!("({line}) Tj T*\n"));
        }
        text.push_str("ET\nq 468 0 0 300 72 60 cm /Im1 Do Q");
        obj(
            &mut out,
            &mut offsets,
            c,
            format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len()).as_bytes(),
        );
        let mut img = Vec::with_capacity(iw * ih * 3);
        let base = r.below(200) as u8;
        for y in 0..ih {
            for x in 0..iw {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                let n = (s & 15) as u8;
                img.push(base.wrapping_add((x / 8) as u8).wrapping_add(n));
                img.push(base.wrapping_add((y / 8) as u8).wrapping_add(n));
                img.push(base.wrapping_add(((x + y) / 16) as u8));
            }
        }
        let mut body = format!("<< /Type /XObject /Subtype /Image /Width {iw} /Height {ih} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length {} >>\nstream\n", img.len()).into_bytes();
        body.extend_from_slice(&img);
        body.extend_from_slice(b"\nendstream");
        obj(&mut out, &mut offsets, im, &body);
    }
    let n = 3 + pages * 3 + 1;
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {n}\n0000000000 65535 f \n").as_bytes());
    for off in &offsets[1..n] {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}
