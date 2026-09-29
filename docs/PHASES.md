# Jess Notes — Phase Plan & Progress Tracker

Source of requirements: `docs/SPEC.md`. Source of design: `docs/DESIGN.md` (approved 2026-09-29, decisions D1–D11 accepted).
Update the **Status** line and tick boxes as work lands. Each phase ends with: all tests green, a commit (and push to `origin/main`), and a short summary appended to the "Phase log" at the bottom of this file.

| Phase | Scope | Status |
|---|---|---|
| 0 | Design doc | ✅ done (approved 2026-09-29) |
| 1 | core + server + sync protocol + blob store/channel + simulation suite (no UI) | ✅ done |
| 2 | projection, import/export engine, server mirror + git | ✅ done |
| 3 | web app + Dockerfile + DEPLOY.md | ✅ done |
| 4 | images & PDFs in the UI | ✅ done |
| 5 | Tauri Linux | ✅ done |
| 6 | Android | ☐ |
| 7 | iPadOS (prep + docs/MAC.md) | ☐ |
| 8 | performance pass + polish | ☐ |

---

## Phase 1 — core + server + sync + blobs + simulation (no UI)

Deliverables (DESIGN §§2–7, 9, 15, 17):
- [x] Cargo workspace: `core/` (jess-core, features `yrs`, `wasm`), `core/wasm/` (wasm-bindgen facade, builds but minimal), `server/` (bin `jess`).
- [x] `core`: ids (UUIDv7), HLC (§6.1), op model + CBOR codec via `minicbor` (§5.1, §5.3), `apply` (LWW registers, cycle reject, collision suffix, trash cascade/restore/purge, recovered-after-purge) (§6).
- [x] `core`: link/tag/frontmatter extraction (§9.1), resolver (§9.2), rewrite formatter + Invariant R computation (§6.6).
- [x] `core`: sans-IO sync client state machine (pending ops, optimistic view + rebase, cursor handling, quarantine, redirects overlay) and blob transfer state machine (upload queue, chunk bitmap, download priorities P0–P4, eviction rule).
- [x] `core/fixtures/*.json` conformance fixtures (links, tags, maths, embeds `|300x200` and `#page=3&height=600`, resolution, sanitisation) + Rust runner.
- [x] Server: SQLite schema (§4.1) with migrations, single writer task, `synchronous=FULL`, apply loop (§5.2) including link pass, compaction (§5.6), WS protocol + HTTP fallback (§5.3), heartbeats (§5.5).
- [x] Server blob store: chunked resumable upload, complete/verify, Range download, presence endpoint, GC (§7).
- [x] Server auth: setup code / `JESS_ADMIN_PASSWORD`, argon2id, device tokens, pairing codes, rate limiting (§14.1). Admin status endpoint, `/healthz`.
- [x] CLI: `serve`, `integrity-check`, `snapshot`, `reset-password`, `gc --dry-run`; snapshots task (§15); graceful shutdown.
- [x] Deterministic simulation suite (§17.1) — 10k seeds in CI, all invariants asserted.
- [x] Property tests (§17.2), process-level crash tests (§17.3), Yjs↔yrs compatibility fixtures (§17.4; needs a small Node script using `yjs`).
- [x] `docs/PROTOCOL.md` generated/written from core types.
- [x] CI workflow (GitHub Actions): fmt, clippy, tests, sim seeds.

Exit criteria: `cargo test --workspace` green, sim 10k seeds green, crash tests green, commit + push.

## Phase 2 — projection, import/export, mirror + git
- [x] `core::projection` (`exact` and `portable` profiles, §12.1) + sanitisation fixtures.
- [x] `core::import` planner (§12.3): folder + zip streaming walkers, skip rules, `.obsidian/app.json` settings, classification, PDF hidden-by-attachment-folder rule, idempotency matching, report, dry run.
- [x] Export: streamed zip (native), folder export; server `GET /api/admin/export.zip`; server-side import `POST /api/admin/import {zip_hash}`.
- [x] `tests/fixtures/vault/` awkward fixture vault (full list in DESIGN §12.3) + byte-for-byte round-trip CI test (client path and server path).
- [x] Mirror task (§13): separate `mirror-state.db`, debounce, incremental diff, atomic writes, hardlinks (→ reflink → copy fallback), markers, `rebuild-mirror`.
- [x] Git: commit after quiet period / max interval, summary messages, `.gitignore` generation, gc, deploy key generation, push with non-fatal failures, optional LFS.
- [x] Tests: mirror == projection after random op sequences (driven by sim), kill mid-write, rename/delete/collision, mirror-blocked doesn't affect sync latency.

## Phase 3 — web app + Docker + DEPLOY.md
- [x] `ui/` Svelte 5 + TS + Vite SPA, pnpm (via corepack). Backend interface + WebBackend (worker + core WASM + Yjs + IDB) + MemoryBackend (§11.2).
- [x] Cold-start boot record path (§11.7); single active tab (D9, §11.6); service worker (app shell).
- [x] Sidebar: three modes, touch drawer, virtualised ARIA tree, context menu, per-device settings (§11.5).
- [x] Commands + keybinding registries, command palette, quick switcher (§11.4, §11.6).
- [x] Editor: CodeMirror 6 + y-codemirror, live preview, Lezer extensions, renderer registry, maths via lazy KaTeX, wikilinks + autocomplete, transclusion placeholder, text fidelity (D11) (§10).
- [x] Backlinks, tags panel, basic search (sqlite-wasm FTS5, lazy), sync status indicator.
- [x] Setup/login/pairing screens; import/export UI with dry run + report.
- [x] Vitest (parsers against shared fixtures, storage/sync layer), component tests (sidebar modes & timing, tree keyboard, switcher), Playwright for the flows in SPEC "Testing". *Chromium + touch emulation run locally; WebKit is configured for CI (`E2E_WEBKIT=1`) but could not be run in this environment.*
- [x] Bundle budget check in CI (`bench/budgets.json`).
- [x] Dockerfile (multi-stage, git, openssh-client, tini, non-root), docker-compose.yml, `DEPLOY.md` (Coolify steps, sizing, proxy limits, migrating a vault, git remote). *pdfium moves to phase 4 with the derivation subprocess that uses it (DESIGN §22.27).*
- [x] KaTeX-compatibility scan: `ui/scripts/katex-scan.mjs <vault>` written and run on the fixture vault (2/2 spans render). **The owner's vault isn't available to this session — run it on the real vault and send the output.**

## Phase 4 — images & PDFs in the UI
- [x] Paste / insert attachment / OS drop; instant placeholder; worker ingest; Obsidian naming; HEIC→JPEG on Apple (§7.3).
- [x] Image renderer (sizes, no layout shift, display variants, placeholders), image viewer chunk (§10.4).
- [x] Server derivation subprocess: display/thumb variants, pdf-thumb, pdf-text (§8). pdfium in the image.
- [x] Service-worker `/_blob/` route + object-URL fallback (§7.7); IDB blob store (OPFS deferred to phase 8, DESIGN §22.34), quota handling, persist().
- [x] Standalone PDF viewer (PDF.js lazy, range transport, virtualised pages, find, zoom, per-device last page/zoom, lazy outline/thumbnails).
- [x] Embedded PDF renderer (static thumb → live viewer, max 2 live).
- [x] PDF text in search, attachments manager panel, blob progress in sync status.

## Phase 5 — Tauri Linux
- [x] `apps/native` (`jess-native`): the same core natively with rusqlite `app.db` (KV + meta) and `index.db` (FTS5, links, tags, PDF text), file blobs, WebSocket transport with HTTP fallback, blob pump, native folder/zip import and zip/folder export, CORS-free auth requests. Integration tests against an in-process server (two devices syncing notes, a 5 MB attachment with Range, rename link rewriting; import from disk → zip and folder export byte-for-byte on both devices).
- [x] `apps/tauri/src-tauri`: TauriBackend over IPC + Channel (binary bodies for doc updates and blob bytes), `jess-blob://` scheme with Range, erase-on-next-launch, `.deb` (9.6 MB) and AppImage (116 MB, bundles WebKitGTK) both built.
- [x] iOS config: mobile entry point in place. *Can't be compiled on Linux (needs the Apple toolchain; no Android NDK on the dev machine either) — first real compile moves to phases 6/7.*
- [x] WebKitGTK verification via `apps/tauri/e2e/smoke.mjs` (tauri-driver): sign-in, note, image via `jess-blob://`, HEIC fallback, PDF.js viewer, FTS search, server has the data, cold start. CI job `linux-app` added.
- [x] Final pass: WASM rebuilt, `cargo test --workspace` + clippy + fmt, 10k-seed simulation, Vitest, svelte-check, Playwright 22/22, Tauri smoke test on real hardware; DESIGN §22 items 40–47; AGENTS.md.

## Phase 6 — Android
- [ ] First mobile compile of `apps/tauri` (Android target; install the NDK).
- [ ] APK/AAB, photo picker, back gesture closes drawer, keyboard-aware editor, foreground catch-up, older WebView testing, cold-start measurement (DESIGN §16 risk).

## Phase 7 — iPadOS
- [ ] Compile `apps/tauri` for iOS on the Mac (never compiled yet).
- [ ] iOS target config, Pencil/hardware keyboard handling, HEIC paste conversion, `docs/MAC.md` with exact steps for the owner to run on a Mac.

## Phase 8 — performance + polish
- [ ] `tools/vaultgen` seeded generator; all benchmarks in DESIGN §18 wired to CI with thresholds; fix regressions; polish.

---

## Phase log
- 2026-09-29 — Phase 0: `docs/DESIGN.md` written and approved in full (D1–D11). Repo created at github.com/dochmccrum/jess-notes (private).
- 2026-09-29 — Phase 1: Rust workspace (`core`, `core/wasm`, `server`). Core: ids/HLC, op model + CBOR protocol, `apply` (LWW registers, cycles, collision suffixes, trash/restore/purge, recovered-after-purge), link/tag/frontmatter/maths extraction, resolver, Invariant R rewrite formatter + stale-link rule, sans-IO sync client (KV persistence, optimistic view, redirects, quarantine) and blob transfer state machine. Server: SQLite schema, single-writer engine with link pass and compaction, WS + HTTP long-poll sync, resumable chunked blob store with Range, auth (setup code / env password, argon2id, device tokens, pairing, rate limits, revoke), admin status, snapshots, GC, trash retention, CLI (`serve`, `integrity-check`, `snapshot`, `reset-password`, `gc --dry-run`). Tests: 10k-seed deterministic simulation (green; it found and drove fixes for 12 real bugs, recorded in DESIGN §22), property tests, deterministic §6 scenarios, conformance fixtures, Yjs↔yrs both directions, e2e against the real binary (p50 edit→remote latency ≈ a few ms locally), SIGKILL crash tests with `integrity-check --hashes`. `docs/PROTOCOL.md` (golden-checked), CI workflow. Pushed to the session branch `claude/fervent-johnson-qjsqyv` (not `main`: this session may only push its own branch).
- 2026-09-29 — Phase 2: `core::projection` (exact + portable, sanitisation fixtures), `core::import` (folder and zip walkers, skip rules, `.obsidian/app.json`, classification incl. non-UTF-8 notes and the attachment-folder PDF rule, idempotency with ask/overwrite/keep-both/skip, unresolved-links and collision report, dry run), `core::export` + a streaming ZIP64 writer; server `GET /api/admin/export.zip` (streamed) and `POST /api/admin/import {zip_hash}`. Awkward fixture vault (`tests/fixtures/vault`, generator `make_vault.py`) with byte-for-byte round-trip tests for the server path (folder and zip sources), the client path (real sync client) and over HTTP. Mirror (`mirror-state.db`, atomic writes, hardlinks → reflink → copy, markers, startup repair, `rebuild-mirror`, `integrity-check --mirror`) and git (commit after quiet period / max interval with summary messages, excludes, deploy key, non-fatal push with backoff, gc, optional LFS). Tests: mirror == projection in the simulation (every third seed), rename/delete/collision commits, SIGKILL crash test now verifies the mirror, blocked mirror doesn't slow pushes.
- 2026-09-29 — Phase 3: `ui/` Svelte 5 + Vite web app. Sync worker (core WASM + Yjs + IndexedDB, WebSocket with HTTP fallback, calls queued behind init), Backend interface with Web and Memory backends, cold-start boot record, one active tab (Web Locks + BroadcastChannel), service worker app shell. Sidebar in all three modes plus touch drawer, virtualised ARIA tree with context menu, command and keybinding registries, palette, quick switcher, CodeMirror 6 editor with live preview, Lezer extensions, lazy KaTeX, wikilink autocomplete, backlinks, tags, FTS5 search (sqlite-wasm, OPFS), setup/login/pairing, import/export UI with dry run, trash, settings, "erase this device". Tests: 113 Vitest (fixtures, stores, storage layer, commands, sidebar timing, tree keyboard, switcher), Playwright against the real server (setup, sync between two browsers, offline/reconnect, rename rewriting links, search, maths, sidebar modes, in-browser byte-for-byte import→export round trip, perf). Measured: cold start → last note visible ≈ 150 ms, note open 7–23 ms, main bundle 82 KB gz (budget 150). Dockerfile (multi-stage, non-root, tini, `jess health`), docker-compose, `docs/DEPLOY.md`, CI jobs for web, e2e and Docker. The Docker runtime stage couldn't be built in this environment (Debian mirrors blocked); the Rust, UI and pdfium stages were built and verified. WebKit e2e runs in CI only. KaTeX scan script ready; needs the owner's vault.
- 2026-09-29 — Phase 4: attachments end to end. Server derivation subprocess (display/thumb variants, EXIF-oriented; PDF first-page thumbnail and per-page text via pdfium; rlimits, timeout; `derived` rows; pdfium pinned in the image). Clients now keep blob facts (size, mime, dimensions). Web: paste/drop/"Insert attachment" with Obsidian naming and attachment-folder rules, "preparing" placeholders, HEIC→JPEG on Apple; image embeds sized before load, image viewer (pan/zoom/pinch); service-worker `/_blob/` route (local chunks, else authenticated fetch; Range; SVG sandboxed) with object-URL fallback; standalone PDF viewer (PDF.js legacy build, range transport over the blob channel, virtualised pages, find, zoom, outline, thumbnails, remembered page/zoom); PDF embeds (thumbnail → live, max 2); PDF text in search with per-page hits; attachments manager (unreferenced files → trash); offline-attachments policy (everything / on-demand LRU) and quota fallback. Measured: 50-image note opens in 16–21 ms, 5 MB PDF first page in 250–270 ms. Fixed along the way: embeds sharing a paragraph weren't rendered; clicks inside embeds turned them back into source; backlinks/search could show stale index results; two e2e helper races.
- 2026-09-29 — Phase 5 (in progress, committed mid-phase): native backend `jess-native` + Tauri 2 Linux app, built and verified on WebKitGTK with a WebDriver smoke test; `.deb` builds. Server `serve` moved into the library (`jess_server::serve`) so native tests run it in-process. Bugs found and fixed: a download the server couldn't serve yet was retried in a tight loop (stalled the web worker; now parked until the blob is present or the client reconnects); a blob whose size wasn't known yet was queued with size 0 forever (now skipped until the server's row gives the size); export failed outright on an unavailable attachment (now skipped and listed in EXPORT-REPORT.txt); Tauri init snapshot could overwrite newer status/entries events (left the app stuck on "Syncing"); the cold-start record kept a stale last note when the app closed without `pagehide`; native background work spawned from the GUI thread panicked. Measured (WebKitGTK, Xvfb software rendering, release build): cold start → note visible 306–380 ms, of which ~120 ms is WebKit loading the entry scripts (SPEC target <300 ms desktop: re-measure on real hardware with `SMOKE_TIMINGS=1`); PDF first page 300–500 ms. Before this commit: workspace tests + 2k-seed simulation green, Playwright 22/22 green, native integration tests green, smoke test green; the final rerun after the last core change is still to do.
- 2026-09-30 — Phase 5 closed: final pass on the owner's Fedora machine after the last core changes. WASM rebuilt; fmt, clippy (all targets, incl. the Tauri app), `cargo test --workspace` (with pdfium) and the 10k-seed simulation (171 s) green; Vitest 113/113, svelte-check 0 errors, Playwright 22/22 (cold start → note 114 ms, 5 MB PDF first page 141–196 ms). Tauri WebDriver smoke test green on real hardware with WebKitGTK: cold start → note visible **165–171 ms** (SPEC desktop target <300 ms met), PDF first page 133 ms. `.deb` (9.6 MB) and AppImage (116 MB) both build. DESIGN §22 items 40–47 record the phase's implementation choices. The iOS/Android build of the app crate can't be compiled here (Apple toolchain / NDK); it's the first task of phases 6 and 7. Bundle sizes unchanged: main 82.9 KB gz (enforced budget 150); codemirror, worker+wasm and sqlite remain over their *tracked* budgets for phase 8.
