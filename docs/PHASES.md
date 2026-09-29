# Jess Notes — Phase Plan & Progress Tracker

Source of requirements: `docs/SPEC.md`. Source of design: `docs/DESIGN.md` (approved 2026-09-29, decisions D1–D11 accepted).
Update the **Status** line and tick boxes as work lands. Each phase ends with: all tests green, a commit (and push to `origin/main`), and a short summary appended to the "Phase log" at the bottom of this file.

| Phase | Scope | Status |
|---|---|---|
| 0 | Design doc | ✅ done (approved 2026-09-29) |
| 1 | core + server + sync protocol + blob store/channel + simulation suite (no UI) | ✅ done |
| 2 | projection, import/export engine, server mirror + git | ⏳ next |
| 3 | web app + Dockerfile + DEPLOY.md | ☐ |
| 4 | images & PDFs in the UI | ☐ |
| 5 | Tauri Linux | ☐ |
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
- [ ] `core::projection` (`exact` and `portable` profiles, §12.1) + sanitisation fixtures.
- [ ] `core::import` planner (§12.3): folder + zip streaming walkers, skip rules, `.obsidian/app.json` settings, classification, PDF hidden-by-attachment-folder rule, idempotency matching, report, dry run.
- [ ] Export: streamed zip (native), folder export; server `GET /api/admin/export.zip`; server-side import `POST /api/admin/import {zip_hash}`.
- [ ] `tests/fixtures/vault/` awkward fixture vault (full list in DESIGN §12.3) + byte-for-byte round-trip CI test (client path and server path).
- [ ] Mirror task (§13): separate `mirror-state.db`, debounce, incremental diff, atomic writes, hardlinks (→ reflink → copy fallback), markers, `rebuild-mirror`.
- [ ] Git: commit after quiet period / max interval, summary messages, `.gitignore` generation, gc, deploy key generation, push with non-fatal failures, optional LFS.
- [ ] Tests: mirror == projection after random op sequences (driven by sim), kill mid-write, rename/delete/collision, mirror-blocked doesn't affect sync latency.

## Phase 3 — web app + Docker + DEPLOY.md
- [ ] `ui/` Svelte 5 + TS + Vite SPA, pnpm (via corepack). Backend interface + WebBackend (worker + core WASM + Yjs + IDB) + MemoryBackend (§11.2).
- [ ] Cold-start boot record path (§11.7); single active tab (D9, §11.6); service worker (app shell).
- [ ] Sidebar: three modes, touch drawer, virtualised ARIA tree, context menu, per-device settings (§11.5).
- [ ] Commands + keybinding registries, command palette, quick switcher (§11.4, §11.6).
- [ ] Editor: CodeMirror 6 + y-codemirror, live preview, Lezer extensions, renderer registry, maths via lazy KaTeX, wikilinks + autocomplete, transclusion placeholder, text fidelity (D11) (§10).
- [ ] Backlinks, tags panel, basic search (sqlite-wasm FTS5, lazy), sync status indicator.
- [ ] Setup/login/pairing screens; import/export UI with dry run + report.
- [ ] Vitest (parsers against shared fixtures, storage/sync layer), component tests (sidebar modes & timing, tree keyboard, switcher), Playwright (Chromium + WebKit) for the flows in SPEC "Testing".
- [ ] Bundle budget check in CI (`bench/budgets.json`).
- [ ] Dockerfile (multi-stage, pdfium, git, openssh-client, tini, non-root), docker-compose.yml, `DEPLOY.md` (Coolify steps, sizing, proxy limits, migrating a vault, git remote).
- [ ] Run a KaTeX-compatibility scan against the owner's vault maths (DESIGN §20.6) and report.

## Phase 4 — images & PDFs in the UI
- [ ] Paste / insert attachment / OS drop; instant placeholder; worker ingest; Obsidian naming; HEIC→JPEG on Apple (§7.3).
- [ ] Image renderer (sizes, no layout shift, display variants, placeholders), image viewer chunk (§10.4).
- [ ] Server derivation subprocess: display/thumb variants, pdf-thumb, pdf-text (§8).
- [ ] Service-worker `/_blob/` route + object-URL fallback (§7.7); OPFS/IDB blob store, quota handling, persist().
- [ ] Standalone PDF viewer (PDF.js lazy, range transport, virtualised pages, find, zoom, per-device last page/zoom, lazy outline/thumbnails).
- [ ] Embedded PDF renderer (static thumb → live viewer, max 2 live).
- [ ] PDF text in search (per-page results), attachments manager panel, blob progress in sync status.

## Phase 5 — Tauri Linux
- [ ] `apps/tauri` with TauriBackend (IPC + Channels), native core + rusqlite app.db/index.db, file blobs, `jess-blob://` scheme with Range, native folder import/export, AppImage + deb. Keep iOS config compiling.
- [ ] WebKitGTK verification (PDF.js, images, HEIC fallback).

## Phase 6 — Android
- [ ] APK/AAB, photo picker, back gesture closes drawer, keyboard-aware editor, foreground catch-up, older WebView testing, cold-start measurement (DESIGN §16 risk).

## Phase 7 — iPadOS
- [ ] iOS target config, Pencil/hardware keyboard handling, HEIC paste conversion, `docs/MAC.md` with exact steps for the owner to run on a Mac.

## Phase 8 — performance + polish
- [ ] `tools/vaultgen` seeded generator; all benchmarks in DESIGN §18 wired to CI with thresholds; fix regressions; polish.

---

## Phase log
- 2026-09-29 — Phase 0: `docs/DESIGN.md` written and approved in full (D1–D11). Repo created at github.com/dochmccrum/jess-notes (private).
- 2026-09-29 — Phase 1: Rust workspace (`core`, `core/wasm`, `server`). Core: ids/HLC, op model + CBOR protocol, `apply` (LWW registers, cycles, collision suffixes, trash/restore/purge, recovered-after-purge), link/tag/frontmatter/maths extraction, resolver, Invariant R rewrite formatter + stale-link rule, sans-IO sync client (KV persistence, optimistic view, redirects, quarantine) and blob transfer state machine. Server: SQLite schema, single-writer engine with link pass and compaction, WS + HTTP long-poll sync, resumable chunked blob store with Range, auth (setup code / env password, argon2id, device tokens, pairing, rate limits, revoke), admin status, snapshots, GC, trash retention, CLI (`serve`, `integrity-check`, `snapshot`, `reset-password`, `gc --dry-run`). Tests: 10k-seed deterministic simulation (green; it found and drove fixes for 12 real bugs, recorded in DESIGN §22), property tests, deterministic §6 scenarios, conformance fixtures, Yjs↔yrs both directions, e2e against the real binary (p50 edit→remote latency ≈ a few ms locally), SIGKILL crash tests with `integrity-check --hashes`. `docs/PROTOCOL.md` (golden-checked), CI workflow. Pushed to the session branch `claude/fervent-johnson-qjsqyv` (not `main`: this session may only push its own branch).
