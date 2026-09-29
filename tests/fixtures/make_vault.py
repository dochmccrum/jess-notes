#!/usr/bin/env python3
"""Regenerates tests/fixtures/vault/ (the awkward Obsidian fixture vault, DESIGN §12.3).
Empty folders and the 5 MB note are created by the round-trip tests themselves."""
import os, shutil, struct, zlib, json

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "vault")
shutil.rmtree(ROOT, ignore_errors=True)

def w(path, data):
    p = os.path.join(ROOT, path)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, "wb") as f:
        f.write(data if isinstance(data, bytes) else data.encode("utf-8"))

def png(w_, h_, rgb=(200, 30, 30)):
    raw = b"".join(b"\x00" + bytes(rgb) * w_ for _ in range(h_))
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w_, h_, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")

def jpeg_with_orientation(o):
    # Not a decodable image, but a valid JPEG header with SOF0 (dimensions 40x30) and EXIF orientation.
    tiff = b"MM\x00\x2a\x00\x00\x00\x08" + struct.pack(">H", 1) + struct.pack(">HHIHH", 0x0112, 3, 1, o, 0) + b"\x00\x00\x00\x00"
    app1 = b"Exif\x00\x00" + tiff
    sof = struct.pack(">BHHB", 8, 30, 40, 3) + b"\x01\x11\x00\x02\x11\x00\x03\x11\x00"
    return (b"\xff\xd8" + b"\xff\xe1" + struct.pack(">H", len(app1) + 2) + app1
            + b"\xff\xc0" + struct.pack(">H", len(sof) + 2) + sof + b"\xff\xd9")

def pdf(pages, title):
    objs = [b"<< /Type /Catalog /Pages 2 0 R >>"]
    kids = " ".join(f"{3 + i * 2} 0 R" for i in range(pages))
    objs.append(f"<< /Type /Pages /Kids [{kids}] /Count {pages} >>".encode())
    for i in range(pages):
        content = f"BT /F1 24 Tf 72 720 Td ({title} page {i + 1}) Tj ET".encode()
        objs.append(f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {4 + i * 2} 0 R /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> >>".encode())
        objs.append(b"<< /Length %d >>\nstream\n" % len(content) + content + b"\nendstream")
    out = b"%PDF-1.4\n"
    offs = []
    for n, o in enumerate(objs, 1):
        offs.append(len(out))
        out += f"{n} 0 obj\n".encode() + o + b"\nendobj\n"
    xref = len(out)
    out += f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n".encode() + b"".join(f"{o:010d} 00000 n \n".encode() for o in offs)
    out += f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    return out

# Obsidian config and things that must be skipped.
w(".obsidian/app.json", json.dumps({"attachmentFolderPath": "attachments", "newLinkFormat": "shortest", "useMarkdownLinks": False, "alwaysUpdateLinks": True}, indent=2))
w(".obsidian/workspace.json", "{}")
w(".trash/Deleted note.md", "old\n")
w(".DS_Store", b"\x00\x00\x00\x01Bud1")
w("Notes/.DS_Store", b"\x00\x00\x00\x01Bud1")

w("Welcome.md", """---
title: "Welcome: odd frontmatter"
tags: [start, "#quoted", nested/tag]
aliases:
  - Home
  - Start page
empty:
weird: |
  multi
  line
---
# Welcome

Links: [[Projects/Plan]] [[Archive/Plan|the archived plan]] [[Deep note#Heading]] [[日本語ノート]]
Embeds: ![[shared.png|300]] ![[Paper.pdf#page=3]] ![[Paper.pdf#page=3&height=600]] ![](Docs/Report.PDF)
Markdown: [same](Notes/Same%20folder.md) [angle](<Notes/My Note.assets/per-note.png>) ![per note](Notes/My%20Note.assets/per-note.png)
Unresolved on purpose: [[Does not exist]] ![[missing.png]]
> [!note] A callout with a %%comment%% and a footnote[^1]
> $e^{i\\pi} + 1 = 0$ and $$x^2$$ and a block id ^block-1

```dataview
TABLE file.mtime FROM #start
```

[^1]: Footnote text.
""")
w("Unicode/Café NFC.md", "Precomposed é in the name.\n")
w("Unicode/Café NFD.md", "Decomposed e + U+0301 in the name.\n")
w("日本語ノート.md", "日本語のテキスト [[Welcome]]\n")
w("a/b/c/d/e/f/g/Deep note.md", "# Heading\n\nDeep down. [[../../../../../../../Welcome]]\n")
w("Projects/Plan.md", "Project plan ![[diagram.png]]\n")
w("Archive/Plan.md", "Archived plan ![[diagram.png]]\n")
w("Projects/diagram.png", png(4, 3, (10, 200, 10)))
w("Archive/diagram.png", png(3, 4, (10, 10, 200)))
w("Windows note.md", "Line one\r\nLine two\r\n[[Welcome]]\r\n")
w("Mixed.md", "unix\nwindows\r\nold mac\rend\n")
w("BOM note.md", "﻿Starts with a BOM.\n")
w("Latin1.md", "caf\xe9 in Latin-1 (not UTF-8)\n".encode("latin-1"))
w("attachments/shared.png", png(2, 2))
w("attachments/manual.pdf", pdf(1, "Manual"))
w("Notes/Same folder.md", "![[same-folder.jpg]] ![[in-assets.png]] ![](assets/in-assets.png)\n")
w("Notes/same-folder.jpg", jpeg_with_orientation(6))
w("Notes/assets/in-assets.png", png(5, 5))
w("Notes/My Note.md", "![[My Note.assets/per-note.png]]\n")
w("Notes/My Note.assets/per-note.png", png(6, 2))
w("Notes/Sub/Parent link.md", "![up](../same-folder.jpg) [up](<../Same folder.md>)\n")
w("Photos/IMG_0001.JPG", jpeg_with_orientation(1))
w("Docs/Report.PDF", pdf(2, "Report"))
w("Docs/Paper.pdf", pdf(4, "Paper"))
w("orphan.gif", b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;")
for i in range(1, 51):
    w(f"Many/Note {i:02d}.md", f"Note {i} ![[shared.png]]\n")
w("Board.canvas", json.dumps({"nodes": [{"id": "1", "type": "file", "file": "Welcome.md", "x": 0, "y": 0, "width": 400, "height": 400}], "edges": []}))
w("Drawing.excalidraw.md", "---\nexcalidraw-plugin: parsed\n---\n==⚠  Switch to EXCALIDRAW VIEW ==\n\n# Drawing\n```json\n{\"type\":\"excalidraw\"}\n```\n%%\n## Drawing\n%%\n")
w("Case/a.md", "lower\n")
w("Case/A.md", "upper\n")
w("Special/Name with #hash? and [brackets].md", "Odd characters allowed on Linux.\n")
w("Trailing space /inside.md", "Folder name ends with a space.\n")
print("vault written to", ROOT)
