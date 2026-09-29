# Agent handoff — Jess Notes

Read this first. It is written for any coding agent (Claude, Codex, Gemini, …) picking up the project.

## What this is
A fast, local-first, Obsidian-compatible markdown notes app with a self-hosted Rust sync server that also serves the web UI. Clients: web, Linux, Android, iPadOS (Tauri 2). Single user, a few devices.

## Documents (in priority order)
1. `docs/DESIGN.md` — **approved design; follow it.** All decisions D1–D11 in its §0 were accepted by the owner as recommended. Do not re-litigate them.
2. `docs/SPEC.md` — the owner's original brief, verbatim. Requirements and acceptance targets live here.
3. `docs/PHASES.md` — phase checklist, current status, and phase log. **Keep it updated.**

## Current state (2026-09-29)
- Phases 0–1 done (see the Phase log in `docs/PHASES.md`). Phase 1 refinements to the design are in DESIGN §22.
- Next step: Phase 2 (projection, import/export, mirror + git).
- Test commands: `cargo test --workspace`; long simulation `SIM_SEEDS=10000 cargo test --release -p jess-server --test sim -- --nocapture`
  (reproduce one seed with `SIM_SEED=n`, which prints a full trace); crash tests `CRASH_ITERS=20 cargo test -p jess-server --test crash`;
  Yjs compat `cd core/tests/yjs-compat && npm install && node gen.mjs && node verify.mjs`.

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
- Dev machine: Fedora Linux; rustc/cargo 1.93, Node 22, git, docker + podman available. pnpm is **not** installed — enable it via `corepack enable`.
- Deployment target: Coolify (Traefik terminates TLS). CI assumed to be GitHub Actions.
- iPadOS builds need the owner's Mac; produce `docs/MAC.md` instructions in Phase 7.
