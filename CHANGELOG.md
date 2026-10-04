# Changelog

## 1.0.1 — 2026-10-04

- **Linux AppImage:** it closed at once on distributions newer than the build machine (e.g.
  Fedora 44), because it bundled an older NSS than the system's crypto module needs. It now uses
  the system's NSS, like the `.deb`, and no longer carries WebKitGTK, which it never used. It's
  50 MB smaller, and CI runs it on Fedora as well as Ubuntu.
- **Settings → Password:** shows *Changing…* while it works (it takes a moment).

## 1.0.0 — 2026-10-04

The first public release.

- **Notes:** a live-preview markdown editor (CodeMirror 6) with Obsidian-style wikilinks,
  embeds, tags, frontmatter, callouts and KaTeX maths. There are backlinks, a quick switcher, a
  command palette, full-text search (notes and PDF text), and trash with restore.
- **Sync:** local-first on every device. Note text merges without conflicts (Yjs), metadata is
  ordered by the server, and an edit never loses to a delete. Renaming or moving a note rewrites
  the links to it, on the server.
- **Obsidian vaults:** import a folder or a zip (with a dry-run report), export back byte for
  byte, and a plain-files mirror of the vault on the server with optional git history.
- **Attachments:** chunked, resumable uploads, images inline at their size, and a PDF viewer
  that opens a 100 MB scan's first page in ~0.1 s. Each device keeps attachments offline or
  downloads them on demand.
- **Apps:** web; Linux (`.deb`, AppImage) on CEF for 120 Hz; Android. The apps can also keep a
  vault on the device only ("spaces") and move it to a server later. Pairing by link or QR code.
- **Server:** one Docker image (amd64, arm64) with the web app, a `/data` volume, database
  snapshots, `jess import` / `derive-all` / `integrity-check` and the other tools in
  docs/DEPLOY.md, and a ready compose file for Coolify.
- **Performance**, on a 10k-note vault with 20k attachments (DESIGN §18): reload → note
  visible ~0.2 s, opening a note ~15 ms, search ~20 ms, an edit on another device in ~40 ms.
