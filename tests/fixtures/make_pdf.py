#!/usr/bin/env python3
"""Writes a PDF of about `mb` megabytes: `pages` pages of text, the first with a large
uncompressed image (so the first page really needs most of the bytes). Used by perf tests."""
import os
import sys


def make(path, pages=40, mb=5):
    objs = []

    def add(b):
        objs.append(b)
        return len(objs)

    w, h = 1200, max(1, (mb * 1024 * 1024) // (1200 * 3))
    img = bytes((x * 7 + y) % 256 for y in range(8) for x in range(w * 3)) * (h // 8 + 1)
    img = img[: w * h * 3]
    font = add(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
    image = add(b"<< /Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length %d >>\nstream\n" % (w, h, len(img)) + img + b"\nendstream")
    pages_id = len(objs) + 1 + 2 * pages + 1
    kids = []
    for i in range(pages):
        text = b"BT /F1 18 Tf 72 740 Td (Perf page %d) Tj ET" % (i + 1)
        if i == 0:
            text += b" q 468 0 0 400 72 300 cm /Im1 Do Q"
        c = add(b"<< /Length %d >>\nstream\n" % len(text) + text + b"\nendstream")
        p = add(b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 %d 0 R >> /XObject << /Im1 %d 0 R >> >> /Contents %d 0 R >>" % (pages_id, font, image, c))
        kids.append(p)
    add(b"<< /Type /Catalog /Pages %d 0 R >>" % pages_id)
    catalog = len(objs)
    assert add(b"<< /Type /Pages /Kids [%s] /Count %d >>" % (b" ".join(b"%d 0 R" % k for k in kids), pages)) == pages_id
    out = bytearray(b"%PDF-1.4\n")
    offs = []
    for i, o in enumerate(objs, 1):
        offs.append(len(out))
        out += b"%d 0 obj\n" % i + o + b"\nendobj\n"
    x = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
    for o in offs:
        out += b"%010d 00000 n \n" % o
    out += b"trailer\n<< /Size %d /Root %d 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, catalog, x)
    with open(path, "wb") as f:
        f.write(out)


if __name__ == "__main__":
    make(sys.argv[1], int(sys.argv[2]) if len(sys.argv) > 2 else 40, int(sys.argv[3]) if len(sys.argv) > 3 else 5)
