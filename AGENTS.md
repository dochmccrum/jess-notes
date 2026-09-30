# Agent handoff — Jess Notes

Read this first. It is written for any coding agent (Claude, Codex, Gemini, …) picking up the project.

## What this is
A fast, local-first, Obsidian-compatible markdown notes app with a self-hosted Rust sync server that also serves the web UI. Clients: web, Linux and Android (Tauri 2). An iPadOS app is a possibility for later, not in the plan. Single user, a few devices.

## Documents (in priority order)
1. `docs/DESIGN.md` — **approved design; follow it.** All decisions D1–D11 in its §0 were accepted by the owner as recommended. Do not re-litigate them.
2. `docs/SPEC.md` — the owner's original brief, verbatim. Requirements and acceptance targets live here.
3. `docs/PHASES.md` — phase checklist, current status, and phase log. **Keep it updated.**

## Current state (2026-09-30)
- Phases 0–6 done (see the Phase log in `docs/PHASES.md`); implementation decisions beyond the approved design are in `docs/DESIGN.md` §22.
- Next step: Phase 7 (performance + polish). Open items carried from phase 6 are listed there (Android cold start on a real phone, native binary size, web keystroke durability window).
- All work is on `main`.
- Build: `cd ui && corepack enable && pnpm install && pnpm wasm && pnpm build` (needs `wasm-bindgen-cli` 0.2.129, the `wasm32-unknown-unknown` target, and `wasm-opt` from binaryen for the real sizes); server `cargo build -p jess-server`; tests `cargo test --workspace`, `SIM_SEEDS=10000 cargo test --release -p jess-server --test sim`, `cd ui && pnpm test && pnpm e2e` (e2e needs the built UI and `target/debug/jess`; set `JESS_PDFIUM_LIB` for PDF tests — CI shows how to fetch `libpdfium.so`).
- Linux app: `cd apps/tauri && ../../ui/node_modules/.bin/tauri build --bundles deb,appimage`; smoke test `node apps/tauri/e2e/smoke.mjs target/release/jess-notes-app target/debug/jess` (needs `tauri-driver` 2.1.0 and WebKitWebDriver; `SMOKE_TIMINGS=1` prints cold-start timings).
- Android: see `docs/ANDROID.md` (toolchain, build, signing, device smoke test `apps/tauri/e2e/android-smoke.mjs`). Minimum engine Chromium 100, checked with `node ui/e2e/old-chromium.mjs <chrome>`.

## Working rules (from the owner)
- Build phase by phase. End each phase with all tests passing, a commit, a push to `origin/main`, and a short summary appended to the Phase log in `docs/PHASES.md`.
- **Ask the owner before any decision that is expensive to reverse** (data model, sync protocol, rename/link semantics, blob addressing) that is not already settled in DESIGN.md. If you must deviate from DESIGN.md, propose it first, then update DESIGN.md in the same commit once approved.
- Other choices: make a sensible call and record it in DESIGN.md.
- Keep dependencies lean; justify heavy ones (DESIGN §19 lists the approved set).
- Sync correctness is the heart of the project: over-invest in the deterministic simulation and property tests (DESIGN §17).
- Performance targets in SPEC "Non-negotiable quality targets" are acceptance criteria.
- Commit messages: concise; the repo uses `main`.

## Key design points to not get wrong (summary; details in DESIGN.md)
- Metadata = server-ordered op log with per-field HLC LWW registers; server validates and publishes rows; clients predict + rebase (D1).
- Note text = Yjs (clients) / yrs with `OffsetKind::Utf16` (server). Compaction merges updates into one row at the max seq — no log horizon.
- Rename/move link rewriting is done **by the server** in the same transaction, under Invariant R: a rename/move never changes what an existing link resolves to (D2, §6.6). Clients use a local redirects overlay until confirmed.
- Edits never lose to deletes; recovered-after-purge rule (D3, §6.4).
- Blobs: whole-file SHA-256, 4 MiB chunks, resumable/idempotent uploads, never evict locally before server confirms (D4, §7).
- Rust `core` is the single source of truth, compiled natively and to WASM for the web worker (D5). TS duplicates only the Lezer display grammar and a tiny resolver, both tested against `core/fixtures/`.
- Note text stored byte-for-byte including CRLF/BOM; CodeMirror `lineSeparator: "\n"` (D11).

## Environment notes
- Dev machine: Fedora Linux; rustc/cargo 1.93, Node 22, git, docker + podman available. pnpm is **not** installed and Fedora's Node has no `corepack`: use `npx -y pnpm@12.8.1 …`. WebKitGTK dev packages and WebKitWebDriver are installed; binaryen via Homebrew; no Xvfb (the smoke test opens windows on the real display). Android: SDK in `~/Android/Sdk` with NDK 28.2.13676358 (set `NDK_HOME` to it; the shell's default points at a missing 26.1) and AVDs `jess33` (WebView 109), `jess29` (WebView 74), `jess36`; Gradle needs `JAVA_HOME=~/android-studio/android-studio/jbr` (the system Java is a headless JRE 25).
- Deployment target: Coolify (Traefik terminates TLS). CI assumed to be GitHub Actions.
- iPadOS is not in the plan (owner, 2026-09-30); if it's picked up later, builds need the owner's Mac and a `docs/MAC.md`.
