# Jess Notes — Original Product Spec

> This is the product brief exactly as the owner wrote it (2026-09-29). It is the source of requirements.
> `docs/DESIGN.md` is the approved design that implements it; where they differ, DESIGN.md wins
> (those differences were approved as decisions D1–D11).

---

# Project: Jess Notes

Build "Jess Notes": a fast, local-first, markdown note-taking app, essentially a lean Obsidian, with a self-hosted sync server that also serves the web app. Clients: web, Android, iPadOS, Linux (Tauri 2). Target user is me (single user, maybe a few devices), so optimise for speed, reliability, and trivial setup over feature count. I am migrating an existing Obsidian vault that is heavy on PDFs and images, so Obsidian compatibility of note content and attachments is a hard requirement.

## Product scope (MVP)
- Notes are markdown. Folders, create/rename/move (via menu or command; tree drag and drop comes later), delete (with trash + restore).
- Live-preview markdown editor (Obsidian-style: syntax hidden when the cursor isn't on the line). Use CodeMirror 6.
- Note linking with Obsidian-compatible wikilinks (see "Linking").
- Maths with Obsidian-compatible syntax (see "Maths").
- Images and PDFs as first-class content (see "Attachments, images and PDFs"): images embed inline; PDFs open as standalone notes in the file tree and can also be embedded in markdown notes. Pasted or inserted media never clutters the file tree.
- Collapsible left sidebar with a folder/file tree (see "Sidebar").
- Basic instant full-text search (SQLite FTS5 or equivalent, including PDF text), quick-switcher (Ctrl/Cmd+O), command palette (Ctrl/Cmd+P). Advanced search (operators, filters) comes later.
- Tags via #tag (and frontmatter `tags`), shown in a sidebar section or panel.
- Light/dark theme, follows system.
- Import from an Obsidian vault and export to plain files (see "Import and export"). No lock-in.
- Non-goals for now, but the architecture must leave room for them (see "Future-proofing"): tabs/split panes, tree drag and drop, PDF annotation/highlighting, OCR, image editing/annotation, note transclusion (`![[Note]]` rendering), Excalidraw, Mermaid, recipe notes with scalable quantities, metadata/properties UI, plugins, graph view, canvas, real-time multi-user collaboration, publishing, end-to-end encryption.

## Linking
- Support Obsidian link syntax exactly: `[[Note name]]`, `[[Note name|alias]]`, `[[Note name#Heading]]`, `[[folder/Note name]]`, `[[file.pdf]]`, `[[file.pdf#page=3]]`, and `![[embed]]`. Store link text verbatim in the note body (never rewrite it into IDs), so export stays plain Obsidian-compatible markdown.
- Resolution follows Obsidian's rules: match by file name anywhere in the vault (case-insensitive; notes, PDFs and media all use the same resolver); if the name is ambiguous, prefer the shortest/closest path and let `[[folder/Name]]` disambiguate. Relative paths (`../x`) and standard markdown links (`[text](path)`, including URL-encoded and `<angle-bracket>` paths with spaces) resolve too. Unresolved links render distinctly, and clicking one creates the note.
- Typing `[[` opens autocomplete (fuzzy, from the metadata index, <30 ms) covering notes and PDFs (images only when the query looks like a file name/extension). Backlinks panel lists notes that link to or embed the current note, including for PDFs. Unlinked mentions can wait.
- Renaming or moving a note or PDF updates inbound links across the vault automatically, as a single atomic operation that stays correct if other devices edit concurrently. Settle the exact semantics in the design doc (this is a CRDT edge case; be explicit).
- Links inside code blocks/spans are ignored. Parsing is incremental and off the main thread.
- Note transclusion (`![[Note]]`, `![[Note#Heading]]`) is preserved verbatim and shown as a link-style placeholder until it is built later.

## Attachments, images and PDFs

### Model
- Every non-markdown file in the vault is an attachment: an immutable, content-addressed blob (SHA-256, deduplicated in storage) plus a vault path entry (path, hash, size, MIME type, timestamps, image dimensions where applicable, `tree_visible` flag). Path entries sync through the same metadata mechanism as notes, so rename/move/trash races are handled the same way (settle the semantics in the design doc).
- Two classes:
  - **Documents:** markdown notes and PDFs. They appear in the tree, quick switcher, search, link autocomplete and backlinks. A PDF is a note with `kind = pdf` whose content is a blob pointer rather than a Yjs text document; it gets the full rename/move/trash/backlink machinery.
  - **Media:** images and everything else embeddable (audio, video, docx, zip, `.canvas`, etc.). Never shown in the file tree by default.
- Tree visibility: markdown notes are always visible. Imported PDFs default to visible (see import rules for the one exception). A PDF inserted into a note by paste/attach defaults to hidden (embedded only), and images/other media are always hidden by default. Any PDF can be toggled with "Show in file tree" / "Hide from tree" (single and bulk). A per-device setting "Show all attachments in file tree" (default off) is an escape hatch that lists media in the tree.
- Deleting a note never deletes its media (matches Obsidian and avoids data loss).

### Inserting media
- Paste from clipboard (images and files), an "Insert attachment" command with a file picker (including the photo picker on Android/iPadOS), and dropping files from the OS onto the editor. (General drag and drop, such as tree reordering, is still deferred.)
- Insertion feels instant: the embed appears immediately with a placeholder while hashing, storage and upload happen in a worker in the background. Do not recompress images. HEIC/HEIF images pasted or picked on Apple devices are converted to JPEG on ingest so they display everywhere; imported HEIC files are kept byte-for-byte and show a fallback where the platform can't decode them.
- Names follow Obsidian's convention (`Pasted image YYYYMMDDHHmmss.png`), collisions resolved deterministically. The inserted link format follows a setting (default `![[name.png]]`; option for standard markdown links). Content already in the vault reuses its existing blob.
- Files are placed at the vault's attachment location (a setting; adopted from the imported vault's Obsidian config, otherwise default `attachments/` at the vault root). This only affects the exported/mirrored layout, never the tree.

### Images
- Render inline in live preview for `![[img.png]]`, `![[img.png|300]]`, `![[img.png|300x200]]`, `![alt](path)`, `![alt|300](path)`, and remote `http(s)` images (lazy, not proxied, with an offline placeholder). Extensions are matched case-insensitively (`.PNG`, `.JPG`).
- No layout shift: reserve space from the dimensions stored in the metadata index. Decode lazily off the main thread; generate and cache downscaled display versions so huge originals are never fully decoded inline. Honour EXIF orientation. Sanitise SVG.
- Clicking an image opens a simple full-size viewer (zoom/pan; pinch on touch). Distinct placeholders for missing, unresolved and still-downloading images.

### PDFs
- Use PDF.js in a lazy-loaded chunk with its worker (excluded from the critical-path bundle budget; notes with no PDFs pay zero cost).
- **Standalone:** a PDF opens from the tree, quick switcher, or a link (`[[file.pdf#page=3]]` opens at that page) in the workspace pane. Virtualised pages (only visible pages render), HTTP range/streaming loading so a large PDF shows page 1 before it fully downloads, zoom and fit-width, page navigation, text selection and copy, find-in-PDF. Last page and zoom are remembered per device (not synced). Outline and thumbnails are lazy panels.
- **Embedded:** `![[file.pdf]]`, `![[file.pdf#page=3]]`, `![[file.pdf#page=3&height=600]]` and `![](file.pdf)` render as an inline scrollable viewer. Show a static first-page placeholder and initialise the live viewer only when scrolled near the viewport; cap concurrently live viewers and tear down offscreen ones. The note stays fully interactive while PDFs load.
- PDFs are full citizens: tree, quick switcher, rename/move/trash, backlinks (which notes link to or embed this PDF), link autocomplete, and rename-updates-inbound-links.
- Search: extract PDF text in a background, low-priority, resumable job off the main thread and index it for full-text search, with results showing the PDF and page. PDFs with no text layer are simply not searchable (OCR is deferred). The design doc decides where extraction runs and whether the extracted index is derived per device or synced; it must never block import, sync or typing.

### Blob storage and sync
- Blob sync is separate from note sync: metadata (paths, hashes, sizes, dimensions) syncs eagerly through the normal log, while blob bytes transfer lazily. A note is fully usable the instant its text arrives, with media showing progress placeholders.
- Uploads/downloads are chunked (chunks small enough for any reverse proxy or CDN, e.g. ≤8 MB), resumable, hash-verified per chunk and per file, and idempotent (re-sending is always safe). Stream on both ends; never buffer a whole file in memory on the client or the server. Configurable max file size.
- The upload queue is persisted and survives restarts, and appears in the sync status indicator ("uploading 12 of 340"). Never evict or delete a local blob until the server has confirmed it.
- Download priority: blobs for the open note, then visible embeds, then recently opened notes, then everything else in the background. Setting "Offline attachments": everything (default on desktop) or on-demand with an LRU cache and size cap (default on mobile).
- The server serves blobs by content hash with immutable cache headers, HTTP Range support, and authentication. The design doc must decide how `<img>` and PDF.js requests authenticate (short-lived signed URLs, or fetch into blob URLs via the service worker) and justify it. Native apps read blobs straight from local files.
- Storage: server blobs on disk under `/data/blobs` (SQLite holds metadata only). Native clients store blobs as files in the app data directory. The web client uses OPFS with an IndexedDB fallback, requests persistent storage, and handles quota errors gracefully.

### Cleanup
- A lazy-loaded "Attachments" panel lists unreferenced media with size and where-used, and lets me delete them (through trash). The reference index is maintained incrementally from link parsing. No automatic deletion by default (optional auto-purge of unreferenced media after N days, off by default). Server-side GC only removes blobs that no path entry references (trash included) after a retention period, and must be safe under concurrent devices.

## Maths
- Inline maths with `$...$` and display maths with `$$...$$` (on their own lines, multi-line supported; single-line `$$...$$` also allowed). Follow Obsidian's rules so currency text like "$5 and $10" is not treated as maths (opening `$` must be followed by a non-space, closing `$` preceded by a non-space and not followed by a digit). Support escaped `\$`. Never parse maths inside code blocks or inline code.
- Render with KaTeX (fastest option, no network). Live preview behaviour matches the rest of the editor: rendered when the cursor is outside the expression, raw source when the cursor is inside it. Invalid LaTeX shows the source with a subtle inline error, never a crash or a blank line.
- Implement as a CodeMirror 6 extension (custom Lezer markdown extension or equivalent) that is incremental, so typing in a note full of equations stays smooth. Cache rendered output by source string.
- KaTeX and its fonts are a lazy-loaded chunk that loads only when a note contains maths (excluded from the critical-path bundle budget). Bundle fonts locally and cache them in the service worker so maths works offline. Notes with no maths must pay zero cost.
- Build maths as one instance of a general "block/inline renderer registry" for the editor (see "Future-proofing") rather than a one-off hack. Images and PDF embeds use the same registry.
- Tests: parsing edge cases (currency, adjacent `$`, escapes, code spans, multi-line blocks, unterminated `$$`), and a typing-latency benchmark in a maths-heavy note.

## Sidebar (left, collapsible)
- A left sidebar showing the vault as a tree: folders and documents (markdown notes and visible PDFs, with a small type icon), folders collapsible/expandable, folders sorted before files, natural alphabetical order. Media (images and other embedded files) are not shown unless "Show all attachments in file tree" is on. Nested to any depth. Current note is highlighted, and its ancestor folders are auto-expanded on open ("reveal active note").
- Header row with new note, new folder, and collapse-all buttons. Right-click / long-press context menu: new note, new folder, rename, move to, delete, and for PDFs "Hide from tree".
- Expanded/collapsed folder state, sidebar width (drag-to-resize with min/max), and sidebar mode are per-device settings, persisted locally and not synced.
- The tree is virtualised and built from the metadata index only (never note bodies or blobs). Expanding/collapsing a folder must stay <16 ms on a 10,000-note vault with 20,000 attachments.
- Full keyboard support following the WAI-ARIA tree pattern: Up/Down to move, Right/Left to expand/collapse or step into/out of folders, Enter to open, F2 to rename, Delete to trash.
- Three selectable modes (Settings, plus quick toggle), all sharing one component:
  1. Pinned: always visible, editor area resizes beside it.
  2. Shortcut: hidden by default, slides in and out as an overlay via a keyboard shortcut (default Ctrl/Cmd+\, rebindable through a small keybinding registry).
  3. Hover-reveal: hidden by default; moving the cursor into a thin hot zone at the left window edge (~8 px, with a ~100 ms intent delay to avoid accidental triggers) slides it in as an overlay; it hides again shortly after the cursor leaves it (~300 ms grace period, no flicker). Not triggered while dragging a selection or holding a mouse button. The shortcut also works in this mode.
  In Pinned mode the shortcut collapses/expands it.
- Overlay modes must not reflow or shift the editor content. Animate with transforms only (~150 ms), respect `prefers-reduced-motion`, and never cause editor jank.
- Touch (tablet/phone): the sidebar is an off-canvas drawer opened by a left-edge swipe or a menu button, closed by tapping the scrim or swiping back (and the Android back gesture). Hover-reveal is disabled on touch. On wide tablets, offer Pinned.
- Structure the layout shell as sidebar + workspace, where the workspace currently holds a single pane (markdown editor or PDF viewer) but is modelled as a container of panes/tabs so tabs can be added later without restructuring.

## Import and export
- The shared code that turns the vault into files is one projection ("vault to folder layout"), used by export, by the server mirror (below), and by tests. Layout: real folder hierarchy, notes as `<title>.md`, PDFs and media at their vault paths. Filenames are sanitised for cross-platform safety (illegal characters, reserved names, trailing dots/spaces, case-insensitive collisions, Unicode normalisation) with deterministic, documented collision handling.
- Export: (a) zip of the entire vault including attachments, available on every platform and as a server admin endpoint (streamed, not built in memory); (b) on Linux desktop, export directly to a chosen folder. Export never modifies the database.
- Import an Obsidian vault:
  - Sources: a folder (Linux desktop folder picker; web via directory upload) and a zip (all platforms, including Android/iPadOS). Read zips and folders as streams, never loading a multi-GB vault into memory.
  - Preserve note text byte-for-byte: frontmatter, callouts, `%%comments%%`, Dataview/plugin syntax, embeds, block IDs, footnotes. Do not "fix" or rewrite content. Wikilink, embed and tag syntax is already compatible, so no link rewriting.
  - Preserve folder structure and file names. Import file modified/created times as timestamps where available.
  - **Attachments migrate seamlessly from where they are.** Every non-markdown file is imported at its exact vault path (no moving, renaming or flattening; blob dedupe is internal only), so every existing image/PDF link resolves exactly as it did in Obsidian: same-folder images, a shared `assets/` folder, per-note `Note.assets`-style folders, `../` relative paths, URL-encoded markdown links, same-name files in different folders, spaces and Unicode in names, uppercase extensions.
  - Images and other media import as hidden media. PDFs import as standalone PDF notes visible in the tree, with one exception: PDFs inside the vault's configured attachment folder are hidden by default. The import dialog shows how many PDFs that rule hides and lets me flip it before importing; visibility can also be changed in bulk afterwards.
  - Read only the attachment-related settings from `.obsidian/app.json` (`attachmentFolderPath`, `newLinkFormat`, `useMarkdownLinks`) to configure Jess's attachment location and link-format settings; never import the `.obsidian` folder itself.
  - Skip `.obsidian/`, `.git/`, `.trash/`, `.DS_Store` and similar; list skipped items in the report. `.excalidraw.md` files import as normal notes with text preserved. `.canvas` and other unknown files import as hidden media, preserved for export.
  - Index tags and frontmatter (`tags`, `aliases`) for the metadata index; keep the frontmatter itself in the note text.
  - Import is idempotent: re-importing the same vault must not duplicate notes or attachments (match by path + content hash, and ask before overwriting when content differs).
  - Notes are usable as soon as their text has imported; blobs upload in the background (persisted, resumable queue) with per-embed progress placeholders. Show overall progress, run off the main thread in batches so the UI stays responsive, and finish with a report: notes imported, PDFs imported (and how many hidden), images/other attachments imported, skipped, unresolved links and embeds, name collisions. Offer a dry run.
  - Target: a 5,000-note vault (with many thousands of attachments) has all notes usable in well under a minute on desktop; blob upload completes in the background without UI jank.
  - Import runs client-side using shared code (the design doc should decide between the shared Rust `core` via WASM/native or a TypeScript implementation covered by the same fixture suite; prefer a single source of truth) so it is exercised by the normal sync path. Server-side zip upload import is an acceptable alternative for very large vaults if justified.
- Round-trip guarantee: import an Obsidian vault, export it, and the result must match the original files byte-for-byte, attachments included (excluding skipped items). Include a fixture vault with awkward cases (unicode names, deep nesting, same-name notes and images in different folders, huge notes, odd frontmatter, every attachment-location convention above, PDFs embedded with `#page=` and `&height=`, orphaned images, images referenced by many notes) and make this a CI test.

## Server mirror and git tracking
- The server continuously maintains a one-way, read-only mirror of the vault as plain files (`.md`, PDFs and media at their vault paths) in `/data/mirror`, using the same projection code as export. The database and blob store are the only source of truth. Nothing ever reads the mirror back into the system. The design doc should decide how blobs get into the mirror without doubling disk usage (e.g. hardlinks vs copies) and justify it.
- Mirror writes are atomic (temp file + fsync + rename), debounced, and incremental (only changed files are rewritten). Deletions and renames are reflected. It runs as an isolated background task: a mirror or git failure must never block, slow, or corrupt sync. It should retry, log, and surface status in the admin/settings screen.
- Write a clear marker file (e.g. `README-GENERATED.md` and a `.jess-generated` file) at the mirror root stating that the folder is generated and edits will be overwritten. Provide a `rebuild-mirror` command that regenerates it fully from the database.
- Git tracking: the mirror folder is a git repository. Changes are committed automatically after a quiet period (default ~60 s) with a bounded maximum interval, and commit messages summarise what changed. Repo maintenance (gc) runs periodically. Because my vault is heavy on PDFs and images, binaries are excluded from git by default (a generated `.gitignore`; they still appear in the mirror folder) and can be included via `JESS_GIT_INCLUDE_ATTACHMENTS=true`. Evaluate Git LFS for that option in the design doc.
- Optional remote push: configure a remote and the server pushes after commits. Setup should be trivial: the server generates an SSH deploy keypair on first run and shows the public key in the admin screen; push failures are non-fatal and visible. Justify the choice of `git` binary vs libgit2 in the design doc.
- Env vars (all optional): `JESS_MIRROR_ENABLED` (default true), `JESS_GIT_ENABLED` (default true), `JESS_GIT_REMOTE`, `JESS_GIT_COMMIT_INTERVAL`, `JESS_GIT_INCLUDE_ATTACHMENTS` (default false).
- Tests: mirror equals the projection of the database after random operation sequences (including attachment add/rename/delete); kill the process mid-write and verify no partial or corrupt files; rename/delete/collision cases; mirror lag never affects sync latency.

## Frontend stack
- Svelte 5 (runes) + TypeScript + Vite, built as a purely static SPA (no SSR, no SvelteKit unless the design doc justifies it). One UI codebase shared by the web app and all Tauri targets.
- Astro, Next, and other SSR/content-oriented frameworks are explicitly out: this is an offline-first app, not a content site.
- CodeMirror 6 and Yjs are used directly (framework-agnostic). Wrap them in thin Svelte components; keep the editor and sync engine outside Svelte's reactivity so typing never triggers component re-renders.
- Keep state small and fine-grained: file tree and search results hold metadata only, never note bodies or blobs. Virtualise the file tree, search results, and backlinks lists.
- Styling: plain CSS with custom properties (no heavy UI kit, no CSS-in-JS runtime). Hand-build the few components needed.
- Bundle budget: initial JS <150 KB gzipped excluding CodeMirror; code-split rarely used panels (settings, import/export, command palette, attachments manager) and heavy renderers (KaTeX, PDF.js and its worker, image viewer). Lazy-load nothing on the critical path to first note render. Fail CI if the budget is exceeded.
- Register a service worker on web for instant repeat loads and offline use (app shell cached; fall back gracefully where unsupported). Avoid cutting-edge web APIs without fallbacks (WebKitGTK and older Android WebView).
- Accessibility basics: keyboard navigation everywhere, visible focus, proper ARIA for tree/listbox/dialog patterns, alt text for images.

## Non-negotiable quality targets
Treat these as acceptance criteria and add automated benchmarks that fail CI if regressed:
- Cold start to interactive, with last-open note visible: <300 ms desktop, <500 ms mid-range Android. The UI must render from local storage first and never wait on the network.
- Open any note: <50 ms, including a note with 50 embedded images (text first, images fill in progressively with no layout shift). Quick-switcher, link autocomplete and search results: <30 ms on a 10,000-note vault with 20,000 attachments.
- Typing latency: no dropped frames, including in a 1 MB note, a maths-heavy note, and a note with many images and embedded PDFs. Never load all note bodies into memory to show the file tree (metadata only).
- PDFs: first page visible <300 ms after opening a typical 5 MB PDF (blob already local) on desktop; a 500-page, 100 MB PDF opens without loading the whole file and stays under a documented memory cap on mobile.
- Sync: an edit on one online device appears on another in <1 s typically (aim for 100-300 ms). Fully offline-capable; reconnect sync is automatic and silent. On mobile foreground, catch-up completes in well under a second for typical deltas. Large blob transfers never delay note sync.
- Sync must be bulletproof: no data loss, no duplicated or resurrected notes, no corrupted state under any interleaving of offline edits, crashes, retries, dropped connections, or clock skew. No attachment is ever lost or left dangling: a blob is never deleted locally until the server confirms it. If in doubt, keep both versions rather than lose one.

## Architecture (propose changes in the design doc if you have a better idea, with reasoning)
- Monorepo: `server/` (Rust, axum), `core/` (shared Rust crate), `ui/` (Svelte 5 + TypeScript + Vite, shared by web + Tauri), `apps/tauri/` (Tauri 2 for desktop + mobile).
- Sync model: local-first with CRDTs. Text of each note is a Yjs document on clients (y-codemirror binding) and yrs on the server, so concurrent edits merge instead of conflicting. Note metadata (path/title, folder, trashed flag, tree visibility, blob pointer for PDFs and media) is also CRDT-backed or last-writer-wins with hybrid logical clocks. Handle rename/move/delete-vs-edit races explicitly and document the chosen semantics.
- Transport: WebSocket for live sync with automatic reconnect + HTTP fallback. Use heartbeats (ping/pong) so dead connections, including ones silently dropped by proxies, are detected within seconds, with fast exponential-backoff reconnect and immediate catch-up on app foreground or network change. Server keeps an append-only, monotonically sequenced update log; clients sync with a cursor ("give me everything after seq N"). All operations idempotent, so replaying or duplicating a message is always safe. Support batching and compaction/snapshotting of the log so first sync and cold start stay fast. Blob transfer runs on its own channel (chunked HTTP) so it can never head-of-line block the note log.
- Visible sync status indicator in the UI (synced / syncing / offline / error, with pending change count and attachment upload/download progress) so I can trust it at a glance.
- Durability: SQLite (WAL mode) on the server and in each native client, with writes committed before ack. Native clients persist via Rust/SQLite, not just webview storage (mobile OSes can evict it), with blobs as files in the app data directory. The web client uses IndexedDB (and OPFS for blobs). Ask for `navigator.storage.persist()`. Put storage behind a single interface in the UI so web and Tauri (Rust commands) are swappable without touching components.
- Heavy work (Yjs merging of big docs, search indexing, PDF text extraction, image processing, markdown parsing for backlinks/tags, import) runs off the main thread (Web Worker on web; Rust on native) so the UI thread stays free.
- Server also serves the built web app as static files (with correct cache headers: hashed assets immutable, index.html no-cache), so one container = sync + web UI.
- Auth: simple. First visit to a fresh server shows a setup screen to create the account (or read `JESS_ADMIN_PASSWORD` from env). Per-device tokens after login; a "pair a device" flow (link or QR code) for the mobile/desktop apps. Rate-limit login. All traffic assumed behind TLS (Coolify/Traefik terminates it).
- Server-side safety net: automatic periodic snapshots of the SQLite DB (configurable retention) into the data volume, plus an admin "download full export" endpoint, plus the git-tracked mirror above. Provide an `integrity-check` command that verifies database consistency, that every referenced blob exists and matches its hash, and that the mirror matches the database.

## Future-proofing (design for these now, do NOT build them yet)
These are planned later features: search improvements, tabs/split panes, tree drag and drop, PDF annotation/highlighting, OCR, image editing, note transclusion, Excalidraw, Mermaid, dedicated recipe notes with variable quantities, and a metadata/properties UI. Make sure the MVP architecture supports them without data migrations or rewrites:
- Notes carry a `kind` field (default `markdown`; `pdf` is the first specialised kind) so further types (e.g. recipes) can be added later. Unknown kinds must sync and export safely.
- Frontmatter stays verbatim in the note text (source of truth), with a parsed, indexed view in the metadata table for later properties/metadata features.
- The editor has a block/inline renderer registry (maths, images and PDF embeds are the first clients) so Mermaid, Excalidraw, transclusion and recipe blocks can be added as pluggable, lazy-loaded renderers.
- Attachments are content-addressed blobs with vault paths; the data model must allow attaching annotations or sidecar data (e.g. future PDF highlights) to a PDF note without changing the blob.
- Workspace state is modelled as panes/tabs (single pane for now); tree operations (move, reorder) are exposed as commands so drag and drop is just a new input method later.
- The keybinding and command registries are extensible.

## Deployment (Coolify)
- Single multi-stage Dockerfile that builds the Svelte web UI and Rust server and produces a small runtime image (include `git` if the design doc chooses the git binary). Also provide a docker-compose.yml.
- One exposed port, one volume mounted at `/data` (database, blobs, snapshots, mirror, git repo), minimal env vars (all optional except where noted): `JESS_ADMIN_PASSWORD`, `JESS_DATA_DIR`, `PORT`, `JESS_MAX_UPLOAD_MB`, plus the mirror/git vars above.
- `/healthz` endpoint for Coolify health checks. Graceful shutdown that flushes the WAL, in-flight blob writes and pending mirror writes.
- Write DEPLOY.md with exact Coolify steps (new resource from Git repo, Dockerfile build pack, set port, add persistent storage at /data, set domain). Goal: deploy in under 5 minutes. Include disk sizing guidance (blobs plus mirror can roughly double storage), notes on proxy/CDN request limits (chunked uploads keep requests small), how to migrate an Obsidian vault, and how to connect a git remote for the mirror.

## Platform notes
- Tauri 2 targets: Linux (AppImage + deb), Android (APK/AAB), iPadOS. Note the iPadOS build requires macOS + Xcode. Do everything else first, keep the iPad target compiling in config, and tell me exactly what I need to do on a Mac.
- Design the UI responsively: touch-first layouts for tablet/phone (large hit targets, swipe to open sidebar, on-screen-keyboard-aware editor, touch-friendly PDF and image viewers), keyboard-first on desktop. Support hardware keyboards and Apple Pencil/stylus input gracefully on iPad (no drawing needed).
- Sync on app foreground and while active; do not rely on mobile background execution.
- Test against WebKitGTK (Linux) and older Android WebView versions, not just latest Chrome. Verify PDF.js and image decoding (including HEIC handling) on each target.

## Testing (sync is the heart of this, so over-invest here)
- Deterministic simulation tests: N simulated clients + server with random offline periods, message drops/duplication/reordering, restarts mid-write, and clock skew. Include blob upload/download (interrupted, duplicated, resumed chunks) in the simulation. Assert all replicas converge to identical state and no acknowledged write or blob is lost.
- Property-based tests for rename/move/delete/edit races, including link-rewrite-on-rename (notes and PDFs) racing with concurrent edits, and attachment path/visibility changes racing across devices.
- Crash-safety tests (kill the process mid-transaction and mid-blob-write, then verify recovery with no partial blobs).
- Vitest unit tests for wikilink/tag/maths/embed parsing (including `#page=` and `|300x200` syntax) and the UI's storage/sync layer. Component tests for the sidebar tree (all three modes, hover-zone timing, keyboard navigation, media hidden, PDF hide/show) and quick-switcher.
- Import/export round-trip test against the fixture vault (see above) and mirror consistency tests.
- Playwright E2E for web covering setup, import a vault, create/edit/search, maths rendering, pasting an image (appears inline, not in the tree), opening a standalone PDF, embedding a PDF, sidebar modes, and two-browser sync, run in Chromium and WebKit.
- Benchmarks for the performance targets above with a seeded 10k-note vault generator (with 20k attachments including large PDFs), including a cold-start measurement, a note-with-50-images open time, PDF open time and memory, and a bundle-size check.

## How to work
1. First, write `docs/DESIGN.md`: architecture, data model (including the document/media split and blob store), sync protocol (note log and blob channel), conflict semantics (including rename-with-link-rewrite), schema, import/export and mirror design, blob authentication approach, threat model, and the benchmark plan. Include the tradeoffs and alternatives you rejected (e.g. Automerge/Loro, plain-file two-way vault sync, KaTeX vs MathJax, PDF.js vs native PDF viewers, SolidJS/React/SvelteKit/Astro for the frontend). STOP and let me review before writing app code.
2. Then build in phases, committing at the end of each with tests passing and a short summary:
   - Phase 1: core + server + sync protocol including the blob store and blob sync channel, with the simulation test suite passing. No UI yet.
   - Phase 2: vault projection, import/export engine (attachments included), server mirror + git tracking, with round-trip and mirror tests passing.
   - Phase 3: web app (app shell, sidebar with all three modes, editor with wikilinks and maths, backlinks, tags, basic search, import/export UI, sync status) + Dockerfile + DEPLOY.md.
   - Phase 4: images and PDFs in the UI (paste/insert, inline images and viewer, standalone and embedded PDF viewer, PDF text search, attachments manager, blob progress in sync status).
   - Phase 5: Tauri desktop (Linux) with native SQLite persistence, file-based blob storage and native folder import/export.
   - Phase 6: Android.
   - Phase 7: iPadOS (prepare, with Mac instructions).
   - Phase 8: performance pass against the benchmarks, then polish.
3. Keep dependencies lean. Justify any heavy dependency. Prefer boring, well-maintained libraries.
4. Ask me before making decisions that are expensive to reverse (data model, sync protocol, rename/link semantics, blob addressing). Otherwise make sensible choices and note them in the design doc.
