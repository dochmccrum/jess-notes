# Jess Notes — Design

Status: **approved** (2026-09-29). All decisions D1–D11 accepted as recommended.

---

## 0. Decisions that need your sign-off

| # | Decision | Recommendation | Section |
|---|----------|----------------|---------|
| D1 | **Metadata sync model.** File/folder metadata (path, parent, trash, visibility, blob pointer) uses a server-ordered operation log, with per-field last-writer-wins registers stamped by hybrid logical clocks (HLCs). It is not a peer-to-peer tree CRDT. The server validates each op (cycles, name collisions) and publishes the resulting rows. Clients predict locally and rebase. Note **text** remains a Yjs CRDT. | Yes | §5, §6 |
| D2 | **Rename/move link rewriting is done by the server** in the same transaction that accepts the rename, following one invariant: *a rename or move never changes what an existing link resolves to*. The renaming client doesn't edit text itself. Until the server's rewrite arrives, it shows a local redirect overlay, so links keep working offline. | Yes | §6.6 |
| D3 | **Delete vs edit:** edits always survive. An edit to a trashed note keeps the note in trash, and the edit is kept, with a banner on the editing device. When a note is created in or moved into a folder that another device trashed concurrently, that folder chain is restored, but its siblings are not. If an edit arrives for a *purged* note and carries new content, the note is recovered into trash as "Recovered — Name". | Yes | §6.4 |
| D4 | **Blob addressing:** SHA-256 over the whole file, lowercase hex, immutable. Uploads use fixed 4 MiB chunks, and each chunk's SHA-256 is verified. There's no chunk-level dedupe or Merkle tree. | Yes | §7 |
| D5 | **Single source of truth for semantics is the Rust `core` crate.** It covers the op model, link/tag/frontmatter extraction, link resolution, rewrite, projection and sanitisation, the import planner, and a sans-IO sync client state machine. The server and Tauri run it natively. The web app runs it as WASM inside its sync worker. Only two pieces are also written in TS: the CodeMirror/Lezer display grammar and a small main-thread link resolver, both checked against the same JSON conformance fixtures. | Yes | §3, §11 |
| D6 | **Derived data is computed on the server:** PDF text extraction (pdfium, run in a subprocess), image display variants, and the first page of each PDF as a thumbnail. All of it is keyed by blob hash and fetched lazily by clients. For newly pasted content, clients generate thumbnails locally until the server's versions arrive. | Yes | §8 |
| D7 | **Blob auth on the web:** bearer device token only, with no cookies and no signed URLs. `<img>` and PDF.js read blobs through a service-worker route (`/_blob/<hash>/<variant>`) served from OPFS/IDB, falling back to an authenticated network fetch. Tauri uses a custom URI scheme that reads local files. | Yes | §7.7 |
| D8 | **Name uniqueness:** names are stored verbatim (whatever bytes/normalisation the original had). A name must be unique per folder after NFC normalisation, **case-sensitively**, so a Linux vault with `a.md` and `A.md` imports and round-trips. Export can apply a "portable" profile that resolves case-insensitive collisions. New names created in Jess must pass Obsidian's name rules. | Yes | §6.3, §12 |
| D9 | **Web: only one active tab.** A second tab shows "Jess is open in another tab — use here", coordinated with Web Locks. This avoids a multi-tab leader-election layer. | Yes | §11.6 |
| D10 | **First-run setup is protected by a one-time setup code** printed to the server log, unless `JESS_ADMIN_PASSWORD` is set. Without it, whoever reaches a fresh public server first owns it. | Yes | §14 |
| D11 | **Line endings and BOM are stored verbatim** in the Yjs text. CodeMirror uses `lineSeparator: "\n"` with `\r` hidden, so CRLF and mixed-ending files round-trip byte-for-byte. A `.md` file that isn't valid UTF-8 is stored as a read-only blob-backed note. | Yes | §10.6 |

Everything else is my call. It's recorded below with reasoning, and you can override any of it.

---

## 1. Principles

1. **Local-first rendering.** Every screen renders from local storage. The network is only for convergence and is never on the critical path to first paint.
2. **The database and blob store are the only truth.** The mirror, exports, search indexes, thumbnails and PDF text are all projections that can be rebuilt. Nothing reads a projection back in.
3. **Keep both rather than lose one.** Every conflict rule is checked against "can this silently drop user bytes?" A rule that could is replaced with one that keeps both versions and makes that visible.
4. **One implementation of every semantic rule** (D5), exercised by deterministic simulation. The code that ships is the code that's simulated.
5. **Idempotency everywhere.** Every message and request can be replayed, duplicated or reordered safely. Ops are keyed by `(replica_id, op_id)`, Yjs updates are idempotent by construction, and blobs are content-addressed.
6. **Keep the main thread free.** Parsing, merging, hashing, indexing and decoding happen in a worker (web) or in Rust (native). Svelte never re-renders while you type.
7. **Boring dependencies.** Each heavy dependency is listed and justified (§19).

---

## 2. Architecture overview

```
                        ┌──────────────────────────── server (one container) ───────────────────────────┐
                        │  axum HTTP/WS                                                                  │
 web / Tauri clients    │   ├─ /api/sync (WS + HTTP fallback) ──► SyncService ──► SQLite (WAL) /data/jess.db
 ───────────────────────┼─► ├─ /api/blobs (chunked, Range)   ──► BlobStore  ──► /data/blobs/ab/cd/<sha256>
                        │   ├─ /api/auth, /api/admin                                                     │
                        │   ├─ static web UI (hashed assets immutable, index.html no-cache)              │
                        │   └─ /healthz                                                                  │
                        │  background tasks (isolated, can fail without affecting sync):                 │
                        │   ├─ Deriver: pdf text / image variants / pdf thumbs (subprocess, low prio)    │
                        │   ├─ Mirror: projection → /data/mirror (atomic writes, hardlinks) → git commit/push
                        │   ├─ Compactor: merges per-doc Yjs updates                                      │
                        │   ├─ Snapshotter: SQLite backup API → /data/snapshots                           │
                        │   └─ Blob GC                                                                    │
                        └────────────────────────────────────────────────────────────────────────────────┘

 Web client                                            Tauri client (Linux / Android)
 ┌──────────── main thread ─────────────┐              ┌──────────── WebView (same ui/ bundle) ─────────┐
 │ Svelte 5 shell, sidebar, panels      │              │ identical UI                                    │
 │ CodeMirror 6 + y-codemirror (open doc)│             │                                                 │
 │ metadata store (flat, fine-grained)  │              └──────────────┬──────────────────────────────────┘
 └──────────────┬───────────────────────┘                             │ Tauri IPC (commands + Channels, binary)
                │ postMessage (binary)                  ┌─────────────▼──────────── Rust ─────────────────┐
 ┌──────────────▼──────── sync worker ───────┐          │ jess-core (native) + yrs                        │
 │ jess-core (WASM): sync client, ops, links,│          │ rusqlite: app.db (truth) + index.db (derived)   │
 │   resolve, projection, import             │          │ blobs as files in app data dir                  │
 │ Yjs (merging/compaction of stored docs)   │          │ tokio WS client, blob transfer, import/export   │
 │ IndexedDB (truth) · OPFS/IDB (blobs)      │          │ custom URI scheme jess-blob:// for <img>/PDF.js │
 │ sqlite-wasm index.db (derived, lazy)      │          └──────────────────────────────────────────────────┘
 └───────────────────────────────────────────┘
 service worker: app shell cache + /_blob/ route
```

### 2.1 Repository layout

```
core/                 jess-core: model, hlc, ops, apply, links, tags, frontmatter, resolve, rewrite,
                      projection, sanitise, import planner, zip/folder walkers, protocol codec,
                      sans-IO sync client + blob transfer state machines. Features: `yrs`, `wasm`.
core/wasm/            jess-core-wasm: wasm-bindgen facade for the web worker
core/fixtures/        JSON conformance fixtures (links, tags, maths, resolution, sanitisation) used by Rust + Vitest
server/               jess-server (bin `jess`: serve | integrity-check | rebuild-mirror | snapshot | reset-password)
ui/                   Svelte 5 + TS + Vite static SPA (web + Tauri)
ui/src/backend/       Backend interface + WebBackend (worker) + TauriBackend (IPC)
apps/tauri/           Tauri 2 app (src-tauri: Rust, depends on jess-core)
tools/vaultgen/       seeded vault generator for benchmarks
tests/fixtures/vault/ awkward Obsidian fixture vault (round-trip CI test)
tests/e2e/            Playwright
bench/                benchmark harness + budgets.json
docs/                 DESIGN.md, DEPLOY.md, PROTOCOL.md (generated from core types), MAC.md (only if the iPadOS app is picked up)
```

Package manager: pnpm through corepack (it isn't installed on this machine yet, but corepack comes with Node 22). Rust uses a single cargo workspace.

---

## 3. Data model

### 3.1 Entries

Everything in the vault is an **entry**: a row with a stable 128-bit id (UUIDv7, generated by the client). Folders, notes, PDFs, media and unknown future kinds are all entries.

| field | type | semantics |
|---|---|---|
| `id` | uuid | immutable |
| `kind` | text | `folder`, `markdown`, `pdf`, `media`, or any future string (e.g. `recipe`). Unknown kinds sync, store and export untouched. |
| `parent_id` | uuid? | LWW register. `NULL` means the vault root. |
| `name` | text | LWW register. The file name, verbatim, **including its extension** (`Note.md`, `scan.PDF`). |
| `trashed` | {batch_id, at}? | LWW register. `NULL` means live. |
| `tree_visible` | bool | LWW register. Only meaningful for `pdf` (and future document kinds); `markdown` is always visible; `media` is ignored unless the device-level "show all attachments" setting is on. |
| `blob` | hash? | LWW register. The content pointer for `pdf`, `media`, and blob-backed markdown (D11). |
| `created_at`, `modified_at` | ms | LWW. `modified_at` is maintained by the server from doc updates, at most once a minute per doc. |
| `props` | map<string, CBOR> | per-key LWW. Extension point for vault-synced properties of future kinds. |
| `purged` | bool | tombstone. The row is kept forever (≈100 bytes). |
| `clock` | map<field, HLC> | the HLC of each register's winning write |
| `seq` | u64 | server log sequence number of the last change |

**Content** is attached to an entry by kind:

- `markdown`: a Yjs document in slot `body` (a single `Y.Text` named `t`). It holds the exact file text (D11).
- `pdf`, `media`: `blob`. The bytes are immutable; replacing a file means pointing the entry at a new blob.
- **Doc slots** are generic, keyed `(entry_id, slot)`. Future PDF highlights will live in slot `annotations` of the PDF entry (a Yjs doc that records the blob hash it annotates), so the blob never changes. Recipes, Excalidraw sidecars and similar work the same way. Unknown slots sync and are preserved opaquely.

**Documents vs media**: `markdown` and `pdf` are *documents*, which appear in the tree, switcher, search, autocomplete and backlinks. Everything else with a blob is *media*. Future kinds declare their class in a client-side kind registry. Unknown kinds default to *media* behaviour: hidden, preserved, exported.

**Folder visibility in the tree**: a folder is shown if it's empty, or if its subtree contains at least one visible document. Folders whose subtree holds *only* hidden entries (for example `attachments/`, `Note.assets/`) are hidden. Folder counts are maintained incrementally. This is how "attachment location affects the exported layout, never the tree" works.

### 3.2 Blobs

`blobs(hash, size, mime, width, height, orientation, present, first_seen_seq, …)`. Dimensions belong to the *content*, so they're keyed by hash. The client that ingests a file supplies them in its `CreateEntry` op, and the server re-derives and corrects them once the bytes arrive. Blob rows carry `seq` and sync through the same log, so clients learn "this blob is now available on the server".

### 3.3 Settings

- **Vault settings** (synced) are stored in `props` of a reserved entry `id = 00000000-…-0001`, `kind = vault`: `attachmentFolderPath`, `newLinkFormat`, `useMarkdownLinks`, `trashRetentionDays`, `autoPurgeUnreferencedMediaDays` (off), and so on. Per-key LWW.
- **Device settings** (never synced): sidebar mode, width, expanded folders, theme override, offline-attachments mode, keybinding overrides, PDF last page/zoom. Stored locally in the `device` store.

### 3.4 Identifiers

- `vault_id`: random, created at server setup. A client refuses to sync a local store that belongs to a different vault.
- `device_id`: one per paired device (tokens, revocation, admin list).
- `replica_id`: random u64 created whenever a local store is created. If a device wipes its storage, it becomes a new replica. Ops are `(replica_id, op_id)`, where `op_id` is a persisted monotonic counter.
- Yjs `clientID`: random per `Y.Doc` session, which is the Yjs default. The server uses fresh random clientIDs for its own edits.

---

## 4. Schema

### 4.1 Server (`/data/jess.db`, SQLite WAL, `synchronous=FULL`, `foreign_keys=ON`)

```sql
CREATE TABLE meta(key TEXT PRIMARY KEY, value BLOB NOT NULL);          -- schema_version, vault_id, head_seq, hlc
CREATE TABLE entries(
  id BLOB PRIMARY KEY, kind TEXT NOT NULL,
  parent_id BLOB, name TEXT NOT NULL, name_key TEXT NOT NULL,          -- name_key = NFC(name)
  trashed_batch BLOB, trashed_at INTEGER, tree_visible INTEGER NOT NULL DEFAULT 1,
  blob BLOB, created_at INTEGER, modified_at INTEGER,
  props BLOB, clock BLOB NOT NULL, purged INTEGER NOT NULL DEFAULT 0,
  seq INTEGER NOT NULL);
CREATE UNIQUE INDEX entries_live_name ON entries(ifnull(parent_id, x''), name_key)
  WHERE trashed_batch IS NULL AND purged = 0;
CREATE INDEX entries_seq ON entries(seq);
CREATE INDEX entries_parent ON entries(parent_id);
CREATE INDEX entries_blob ON entries(blob);

CREATE TABLE doc_updates(                                              -- the text log
  seq INTEGER PRIMARY KEY, entry_id BLOB NOT NULL, slot TEXT NOT NULL,
  update BLOB NOT NULL, origin_replica INTEGER, merged INTEGER NOT NULL DEFAULT 0);
CREATE INDEX doc_updates_doc ON doc_updates(entry_id, slot, seq);
CREATE TABLE doc_purged(entry_id BLOB, slot TEXT, state_vector BLOB, PRIMARY KEY(entry_id, slot));

CREATE TABLE replicas(replica_id INTEGER PRIMARY KEY, device_id BLOB, last_op_id INTEGER NOT NULL,
                      last_seen INTEGER);                               -- idempotency
CREATE TABLE op_results(replica_id INTEGER, op_id INTEGER, seq INTEGER, result BLOB,
                        PRIMARY KEY(replica_id, op_id));                -- pruned after 30 days
CREATE TABLE rename_history(seq INTEGER, entry_id BLOB, old_parent BLOB, old_name TEXT,
                            new_parent BLOB, new_name TEXT, origin_replica INTEGER, origin_op INTEGER,
                            induced INTEGER NOT NULL);                  -- see §6.6
CREATE TABLE links(src BLOB, slot TEXT, ord INTEGER, start16 INTEGER, end16 INTEGER,
                   syntax INTEGER, embed INTEGER, target TEXT, target_key TEXT,
                   subpath TEXT, resolved BLOB, PRIMARY KEY(src, slot, ord));
CREATE INDEX links_key ON links(target_key); CREATE INDEX links_resolved ON links(resolved);

CREATE TABLE blobs(hash BLOB PRIMARY KEY, size INTEGER, mime TEXT, width INTEGER, height INTEGER,
                   orientation INTEGER, present INTEGER NOT NULL, stored_at INTEGER,
                   unreferenced_since INTEGER, seq INTEGER NOT NULL);
CREATE TABLE uploads(upload_id BLOB PRIMARY KEY, hash BLOB, size INTEGER, chunk_size INTEGER,
                     received BLOB /* bitmap */, created_at INTEGER, device_id BLOB);
CREATE TABLE derived(hash BLOB, kind TEXT, status TEXT, version INTEGER, size INTEGER, error TEXT,
                     PRIMARY KEY(hash, kind));                          -- files under /data/derived

CREATE TABLE account(id INTEGER PRIMARY KEY CHECK(id=1), password_hash TEXT, created_at INTEGER);
CREATE TABLE devices(id BLOB PRIMARY KEY, name TEXT, token_hash BLOB UNIQUE, created_at INTEGER,
                     last_seen INTEGER, revoked_at INTEGER);
CREATE TABLE pairing_codes(code_hash BLOB PRIMARY KEY, expires_at INTEGER, used_at INTEGER);
```

A single global sequence (`meta.head_seq`) orders `entries`, `doc_updates` and `blobs`. Each committed transaction takes a contiguous range. The mirror keeps its own state in a **separate** `/data/mirror-state.db`, so the mirror can never hold locks on the main database (§13).

### 4.2 Clients

The same logical stores on every platform (native: `app.db` via rusqlite; web: IndexedDB object stores):

| store | content |
|---|---|
| `entries` | confirmed server rows (a subset of the columns above) |
| `pending_ops` | ordered local ops not yet confirmed (op body, hlc, known_seq, op_id) |
| `docs` | per `(entry, slot)`: merged confirmed state + list of pending local updates |
| `cursor` | last fully-applied server seq |
| `blobs_local` | hash → {state: `local_only`/`uploading`/`confirmed`, bytes_have (chunk bitmap for partial downloads), last_access, pinned} |
| `upload_queue` / `download_queue` | persisted, prioritised |
| `redirects` | local overlay of unconfirmed renames (§6.6) |
| `boot` | a single record: last-open note id + merged doc state + compact entries snapshot, for cold start |
| `device` | device settings |
| `quarantine` | ops the server rejected (never discarded; visible in settings) |

The derived **`index.db`** (native SQLite; web: sqlite-wasm with the OPFS SAH-pool VFS, lazy-loaded) holds `links`, `tags`, `frontmatter` (as parsed JSON), `fts` (FTS5: notes and PDF pages), and `blob_refs`. It can be deleted and rebuilt at any time.

---

## 5. Sync protocol

### 5.1 Ops

```
Op        = { op_id: u64, hlc: Hlc, known_seq: u64, body: OpBody }
OpBody    = Meta(MetaOp) | Doc { entry, slot, update: bytes }         // Yjs v1 update
MetaOp    = Create { id, kind, parent, name, tree_visible, blob?, blob_info?, created_at?, props? }
          | SetParent { id, parent } | SetName { id, name }            // move = SetParent (+SetName) in one op group
          | SetVisible { id, visible } | SetBlob { id, blob, blob_info }
          | Trash { id } | Restore { batch_or_id } | Purge { id }
          | SetProp { id, key, value } | SetTimes { id, created?, modified? }
Hlc       = (wall_ms: u48, logical: u16, replica_id: u64)             // total order
```

`known_seq` is the server seq the replica had applied when it created the op. Together with same-replica op order, this gives the server enough causality to detect stale links (§6.6). Ops can be grouped (`group_id`): the server applies a group atomically or not at all. A move-and-rename is one group, and so is an import batch.

### 5.2 Server apply loop

The server has one writer task that owns the write connection. For each `Push` it:

1. Opens a transaction and skips ops with `op_id ≤ replicas.last_op_id` (duplicates are answered from `op_results`).
2. Clamps `hlc.wall` to `server_now + 2 s` to limit clock skew (§6.1), and advances the server HLC.
3. Applies meta ops through `core::apply` (LWW per field, then validation: cycle check, name collision → suffix, trash cascade). This emits changed rows with new seqs.
4. Applies doc updates to the cached `yrs` doc (`OffsetKind::Utf16`, which matches Yjs). If an update doesn't decode, the op is rejected as `BadUpdate`, and the client moves it to quarantine instead of deleting it. Each update is appended to `doc_updates`, and the doc is marked dirty for link extraction.
5. **Link pass** (§6.6): flushes link extraction for dirty docs, computes the rewrites needed by renames and moves in this batch and by stale links, applies them as server-authored yrs edits, and appends them to `doc_updates`.
6. Commits (`synchronous=FULL`, so the batch is on disk before any ack), then sends `Ack`, then notifies connection tasks.

The server works in two modes at once: in bulk it processes pushes in arrival order, and per field it resolves by LWW-by-HLC. The result is deterministic because only the server decides. Clients never compute the canonical state on their own; they apply server rows.

### 5.3 Messages (WebSocket binary frames; CBOR via `minicbor`, integer-keyed, unknown fields ignored)

```
C→S  Hello   { proto: 1, vault_id?, replica_id, token, cursor, app_version }
     Push    { ops: [Op] }                          // ≤ 1 MiB per frame; larger pushes are split
     Pull    { from, limit_bytes }                  // explicit catch-up
     DocSync { entry, slot, state_vector }          // repair / full resync of one doc
     Ping    { nonce }
S→C  Welcome { vault_id, head_seq, server_time, min_client_proto }
     Ack     { results: [(op_id, Applied{seq} | Duplicate{seq} | Rejected{reason})] }
     Changes { from, to, entries: [Row], blobs: [BlobRow], docs: [DocUpdate{seq, entry, slot, bytes | Own}], more }
     Pong    { nonce, server_time }
     Error   { code, message, retry_after? }
```

- **Authentication**: the first frame must be `Hello` with the device token, within 5 s. Browsers can't set headers on WebSocket, and tokens must never go in URLs, because URLs end up in proxy logs.
- **Cursor contract**: `Changes{from, to}` means "everything that changed in `(from, to]`". A client applies a `Changes` only when `from == cursor`. Otherwise it sends `Pull{from: cursor}`. Rows whose seq moved later (they were changed again) will turn up later. Every payload is idempotent.
- **Own updates** come back as `Own` placeholders (seq only, no bytes), so a large paste isn't echoed.
- **Live push**: after each commit, every connection task reads `(its_cursor, head]` from the DB. Lag can't lose messages, because the DB is the queue.
- **Catch-up**: rows first (all `entries`/`blobs` with `seq > from`; the full metadata of 30k entries is about 3 MB), then doc updates in pages of about 2 MB. The cursor is persisted only after the last page. Pages resume from a persisted sub-cursor. A client that's missing everything (a new device) receives compacted snapshots, which is the same code path.
- **HTTP fallback**: `POST /api/sync` takes `{hello, ops, cursor, wait_s≤25}` and returns `{acks, changes}` with long-polling. It's used when the WebSocket fails 3 times in a row, or behind proxies that break upgrades.

### 5.4 Client pipeline

- **Local edit**: the main-thread Y.Doc emits an update. It goes to the worker/Rust, which coalesces updates per doc for 30 ms (`Y.mergeUpdates`), then **persists** them (`pending` + `docs`), then pushes. Meta intents are applied optimistically: `view = confirmed ⊕ pending` (the same `core::apply`, run locally).
- **Receive**: `Changes` is applied to the confirmed state in one local transaction, together with the cursor. Pending ops with `seq ≤ cursor` are dropped. The optimistic view is rebased. If the server's result differs from the prediction (for example, it added a collision suffix), the UI simply updates.
- **Pending ops are never dropped** before they're confirmed. They're resent after reconnect (the server dedupes them). Rejected ops go to `quarantine`.
- **Status** comes from the same state: `synced`, `syncing(n pending)`, `offline(n pending)`, `error(reason)`, plus `uploading i of n`, `downloading i of n`.

### 5.5 Liveness

- The client sends `Ping` every 10 s while the app is in the foreground. If a `Pong` doesn't arrive within 5 s, the socket is treated as dead and reconnects. The server sends WS pings every 20 s and drops a connection after 45 s of silence.
- **Reconnect**: immediate the first time, then 250 ms, 500 ms, 1 s, 2 s, 4 s, 8 s, 15 s (the cap while in the foreground; 60 s in the background). ±20% jitter. The backoff resets once a `Welcome` arrives.
- The `online` event, `visibilitychange` to visible, a Tauri app resume, and an Android network change each trigger an immediate probe (a `Ping` with a 2 s timeout, reconnecting if it fails) followed by a `Pull`.
- The client never relies on background execution on mobile.

### 5.6 Compaction

The compactor merges, per doc, every `doc_updates` row with `seq ≤ X` into **one row at seq X** (`yrs` merge, or encode state as update) once the doc has more than 200 rows or the rows are older than an hour. A client with cursor `c < X` receives the merged row, which is a superset. That's safe because Yjs apply is idempotent. A client with `c ≥ X` already has all of it. **Compaction therefore never creates a "log horizon" that forces a full resync.** Clients compact their local `docs` the same way (in Yjs in the worker, or yrs natively) once pending updates are confirmed.

### 5.7 Durability summary

| event | guarantee |
|---|---|
| keystroke | persisted locally within about 30 ms (a crash before that can lose ≤30 ms of typing; every persisted keystroke is safe) |
| ack | server has committed with `synchronous=FULL` |
| local blob | never evicted or deleted until the server reports `present` for that hash |
| server crash mid-batch | the transaction rolls back; the client resends; dedupe by op id |
| client crash mid-apply | the local transaction rolls back; the cursor is unchanged; the `Changes` are re-pulled |

---

## 6. Conflict semantics

### 6.1 Registers and clocks

Each field is an LWW register ordered by HLC `(wall, logical, replica)`. A write takes effect only if its HLC is greater than the field's current `clock`. HLCs advance on every received server message. So **a write made after seeing another write always wins** (causal order is respected whatever the wall-clock skew). Among truly concurrent writes, the later wall time wins. The server clamps incoming wall times to `server_now + 2 s`, so a device with a clock far in the future can't win every conflict. A device whose clock is behind only loses races that were actually concurrent.

### 6.2 Moves and cycles

`parent_id` is a register. When the server applies a `SetParent` that would create a cycle (possible when devices concurrently move A into B and B into A), it **rejects** it and records `Rejected{Cycle}`. The client's optimistic move is rolled back, and a toast explains why. Moving an entry under a trashed or purged parent is also rejected (with the same keep-data semantics as below).

### 6.3 Name collisions

Uniqueness is on `(parent, NFC(name))` among live entries (D8). If a create, rename, move or restore would collide, the server keeps the op but gives the **later-applied** entry a deterministic suffix, following Obsidian: `Name 1.md`, `Name 2.md`, …, taking the smallest free number. This is an *induced rename*, recorded in `rename_history` with `induced=1`, so the link logic treats it like any other rename (§6.6). Two offline devices that both create `Untitled.md` end up with `Untitled.md` and `Untitled 1.md`. Both are kept and nothing is merged silently.

Import is the exception. It uses exact-path matching and asks you what to do (§12.3).

### 6.4 Trash, restore, purge vs edits (D3)

- **Trash** of an entry or folder is one op. The server stamps `trashed_batch` on the entry and on every live descendant, as one atomic batch. **Restore** of a batch restores exactly those entries. If a restored path now collides, the restored entry gets a suffix.
- **Edit to a trashed note**: the edit is applied (text is never lost), and the note stays in trash. The editing device shows "This note was moved to trash on another device — Restore / Keep in trash". The rule is *delete wins for location, edit wins for content*.
- **Create in, or move into, a concurrently trashed folder** (the op's `known_seq` is before the trash): the server restores the ancestor folders of the new entry, and **only those**, and records why. The new entry stays visible where you put it, and the rest of the trashed folder stays in trash.
- **Purge** (emptying trash, manually or after the `trashRetentionDays` setting, default 30) deletes the doc updates. It keeps the tombstone row and stores the doc's final **state vector** in `doc_purged`. If a later doc update for that entry contains operations *not covered by* the purged state vector (a device edited offline, didn't know about the purge, and came back), the server **recovers** the entry into trash as `Recovered — <name>`, holding the full offline update. Content that's already covered is ignored, so a stale replica replaying old state can't resurrect a note. For blobs, the blob stays referenced by the tombstone for the GC retention period (§7.8).

### 6.5 Text

Concurrent text edits merge through Yjs, with no "conflicts". Formatting-sensitive edge cases, such as two devices editing the same heading, produce the Yjs interleaving and never lose characters. That's the accepted and expected CRDT behaviour.

### 6.6 Rename/move with link rewriting (D2)

**Invariant R.** *When a rename or move of entry(ies) S is applied, every existing link in the vault that resolved to an entry `E` before the op must resolve to `E` after the op, unless `E` was trashed or purged.*

This one rule covers all of these:

- Renaming a note or PDF: `[[Old]]`, `[[Old|alias]]`, `[[Old#Heading]]`, `![[Old.pdf#page=3]]`, `[t](Old%20name.md)`, `[t](<../x/Old name.md>)`.
- Moving a note: inbound path-qualified links, **and the moved note's own relative links**, whose meaning would otherwise change.
- Moving a folder: every inbound path link to anything inside it, and relative links from inside it to outside.
- A move that makes a bare name ambiguous (you move a second `Name.md` closer to some linkers): the affected links are rewritten to a path so they keep pointing where they did.
- Links that were *unresolved* before are left alone. If a rename makes `[[New]]` resolve, that's what Obsidian does too.

**Who rewrites.** The server does, inside the same transaction as the rename, so the rename and all its rewrites go out as **one atomic `Changes` range**. A client applies a `Changes` page in one local transaction, so no device ever sees the new name without the rewritten links, or the reverse. The rewrites are ordinary yrs edits: they replace only the *target substring* of each link (with alias, subpath and size suffix kept byte-for-byte), and they're authored against the server's current text after every update in the same push has been applied.

**Why not on the client** (the alternative I rejected): if the renaming client rewrote links itself, two devices renaming the same note concurrently (A→B and A→C) would both delete `A` and insert their own names. Yjs would merge that into `[[BC]]`. Server-side rewriting happens in a single total order, so the second rename rewrites `[[B]]`→`[[C]]` cleanly.

**Offline renames.** The client applies the rename optimistically and adds `redirects[old location] = entry_id`. The local resolver checks redirects when a link would otherwise resolve differently or fail, so links keep working, backlinks stay correct, and clicking goes to the right note. Link text updates on reconnect. The redirect is removed once the server's rename is confirmed. (Known limitation: an export taken *on that device while it's offline* contains the old link text. I'll document it.)

**Formatting the new link text.** Syntax and style are preserved: wikilink vs markdown link, whether the extension was written, bare name vs vault-absolute path vs relative path, and in markdown links the URL-encoding style (`%20` vs `<…>`) and case of the extension. A bare name stays bare if it's unambiguous after the op. Otherwise it becomes the vault-absolute path, following Obsidian's "shortest path when possible".

**Stale links from devices that hadn't seen the rename.** Suppose device Y is offline, adds `[[Old]]` to some note, and syncs after device X renamed Old→New. For each *newly introduced* link in a doc update (in the link index after but not before), the server looks for `rename_history` rows with `seq > op.known_seq` whose *old* location the link's target matches (using the resolver rules applied to the old location), excluding renames from the same replica with a lower op id, unless they're `induced`. If exactly one entry matches, the link is rewritten to point at that entry. Otherwise it's left alone, as a visible unresolved or differently-resolved link. The same rule handles collision suffixes (the device's links to `[[Untitled]]` follow its note to `Untitled 1`). It's best-effort by design: the worst outcome is a visibly unresolved link, never lost text.

**Concurrent edits inside a link being rewritten** (a device edits the characters of `[[Old]]` while the server rewrites it) can produce Yjs-interleaved text such as `[[NewOld-typo]]`. Nothing is lost, the link shows as unresolved, and it's rare for a single user. Accepted and documented.

**Cost.** The server keeps the `links` table (target key, resolved id, UTF-16 range) up to date. Candidates for a rename are the links whose `target_key` equals the old or new basename, links whose `resolved` is in S, and links from sources inside S. Each candidate is re-resolved against the after-state, so the work is proportional to the affected links, not the vault size.

### 6.7 Attachments

Paths, visibility and blob pointers are registers on the entry, so attachment races follow §6.1–6.4 exactly:

- One device renames `img.png` while another moves it: both apply, and links are rewritten by Invariant R.
- One device hides a PDF while another renames it: both apply.
- One device replaces a PDF's content (`SetBlob`) while another trashes it: it's trashed with the new blob, so both are kept (trash never deletes).
- Two devices paste identical bytes: two entries, **one blob** (dedupe by hash), and two files in the export. Obsidian would do the same.
- Deleting a note never deletes media.

---

## 7. Blob store and blob channel

### 7.1 Addressing (D4)

`hash = SHA-256(file bytes)`, stored as 32 raw bytes and shown as lowercase hex. Server path: `/data/blobs/ab/cd/abcd…` (0444 permissions, owned by the server's non-root uid). Temp files go in `/data/blobs/.tmp/`, on the same filesystem, so rename is atomic.

### 7.2 Upload (resumable, idempotent, streamed)

```
POST /api/blobs/{hash}/uploads   {size}           → 200 {present: true}                        (dedupe)
                                                   | 201 {upload_id, chunk_size: 4 MiB, received: bitmap}
PUT  /api/blobs/{hash}/uploads/{id}/chunks/{i}    header X-Chunk-SHA256; body ≤ 4 MiB
                                                   → server streams to .tmp/{id} at offset i*chunk (pwrite),
                                                     verifies the chunk hash, fsyncs, sets the bit. Re-PUT of a set bit → 200 no-op.
POST /api/blobs/{hash}/uploads/{id}/complete      → streams the temp file through SHA-256; on match: fsync, rename
                                                     into place, fsync dir, blobs.present=1 (new seq → clients learn it)
                                                   → mismatch: 422, upload discarded, client re-hashes the source
```

- A 4 MiB chunk plus headers stays well under common proxy limits: Traefik has no default limit, Cloudflare's free plan allows 100 MB, and nginx defaults to 1 MB. DEPLOY.md warns about nginx.
- `JESS_MAX_UPLOAD_MB` (default 2048) is checked on `POST`.
- Neither side holds a whole file in memory. Clients read the source in 4 MiB slices (web: `Blob.slice`; native: file reads).
- Up to 2 uploads run in parallel with 2 chunks each. They are a separate HTTP channel with their own concurrency limit, so they never share the sync WebSocket (no head-of-line blocking).
- Expired uploads (more than 7 days untouched) are removed by GC.

### 7.3 Ingest on the client

A pasted, dropped or picked file goes through these steps:

1. The editor inserts the embed text *immediately*, using the name decided in step 2, and renders a "preparing" placeholder.
2. The worker (or Rust side) streams the file into local blob storage while computing SHA-256. It reads image dimensions and EXIF orientation from the header (the `imagesize` crate plus a small EXIF reader in core). If the device is Apple and the file is HEIC or HEIF, it converts to JPEG at quality 0.92 using the platform decoder. That's the only recompression, and the spec allows it.
3. It sends a `Create` op (with `blob_info`) and enqueues the upload in the persisted queue.

Names follow Obsidian: `Pasted image YYYYMMDDHHmmss.png`. If a name collides in the target folder, ` 1`, ` 2`… is appended before the extension, which is deterministic. If the bytes already exist in the vault, the existing blob is reused automatically, but there's still a new entry (and path). *Alternative I considered:* linking to the existing entry instead. I rejected it because the note's embed would then point at a file somewhere else in the vault, which is surprising and differs from Obsidian.

### 7.4 Download

`GET /api/blobs/{hash}` and `GET /api/blobs/{hash}/derived/{kind}` return `Cache-Control: private, max-age=31536000, immutable` and `ETag: "<hash>"`, support `Range`, and stream from disk.

The client downloads in 4 MiB-aligned ranges into a sparse local file with a chunk bitmap. PDF.js range requests reuse whatever chunks are already present. Download priority is a single queue with five levels, re-ranked on navigation:

`P0` open document and its visible embeds → `P1` other embeds in the open note → `P2` recently opened notes → `P3` background prefetch ("Offline attachments: everything") → `P4` derived text for search.

### 7.5 Local storage

| platform | where | notes |
|---|---|---|
| Tauri (all) | `<app_data>/blobs/ab/cd/<hash>` | fsync + rename. Read straight from disk. |
| Web | OPFS `blobs/<hash>` (SyncAccessHandle in the worker); IndexedDB fallback (4 MiB chunk records) | calls `navigator.storage.persist()`. On `QuotaExceededError` it switches to on-demand mode, evicts confirmed LRU blobs, and shows a banner. It never evicts `local_only` or `uploading` blobs. |

**Offline attachments** is a device setting. `everything` is the default on desktop. `on-demand` is the default on mobile and the web, with an LRU cap that defaults to 2 GB on mobile and 1 GB on the web. Derived display variants are always kept, because they're small.

### 7.6 Reconciliation

When a client starts, and every 10 minutes after that, it compares its `local_only` blobs against the server (`POST /api/blobs/presence` with a list of hashes) and re-queues any the server lacks. The server exposes `missing` (blobs referenced by live entries but not present) in the admin screen, together with the devices that referenced them.

### 7.7 Authenticating `<img>` and PDF.js (D7)

- **Web**: the service worker serves `/_blob/{hash}/{variant}` (variant is `orig`, `display`, or `thumb`). It reads from OPFS/IDB. On a miss it fetches from the server with `Authorization: Bearer <token>` (the token is read from IDB) and streams the response to the page while also writing it into local storage. It supports `Range`. SVG responses get `Content-Security-Policy: sandbox` and are only ever shown through `<img>` (§10.4). If the SW isn't active yet (the first ever visit) or isn't supported, the `BlobUrlResolver` falls back to `fetch` → `URL.createObjectURL`, with revocation handled by an LRU.
- **PDF.js** always uses a custom `PDFDataRangeTransport` backed by the same blob reader (local chunks first, then authenticated range fetch), so streaming works identically on every platform.
- **Tauri**: an async URI scheme handler, `jess-blob://localhost/{hash}/{variant}` (on Android: `http://jess-blob.localhost/…`), reads local files and supports Range. If a blob isn't local it triggers a P0 download and streams it.

Why this approach: tokens never appear in URLs (URLs end up in logs, referrers and history). There are no cookies, so there's no CSRF surface. Nothing expires in the middle of a 100 MB PDF stream. It works offline through the same path. *Rejected:* signed URLs (they leak into logs, expire mid-stream and depend on clocks) and cookie auth (CSRF, and it doesn't help Tauri).

### 7.8 Garbage collection

- Client side: the "Attachments" panel shows unreferenced media, meaning media not referenced by any link in a live **or trashed** note, using `blob_refs` from the index. Deleting them moves them to trash. Optional auto-purge is off by default.
- Server side: a blob file is deleted only when **all** of these hold:
  1. No entry row references it (live, trashed, or a purged tombstone younger than the retention period).
  2. It has been unreferenced (`unreferenced_since`) for at least `JESS_BLOB_RETENTION_DAYS` (default 30, and always greater than the snapshot retention plus 7 days, so **restoring any snapshot never refers to a deleted blob**).
  3. No upload is in progress for it.
- GC marks and deletes under the writer lock in small batches. An entry that references a hash always clears `unreferenced_since` in the same transaction, so GC can't race a new reference. If an entry referencing a blob that was already deleted arrives (a device that was offline for longer than the retention period), the blob is `missing`. The creating device still has it locally, because of the eviction rule, so it gets re-uploaded.

---

## 8. Derived data

| derived | computed where | keyed by | synced how |
|---|---|---|---|
| link/tag/frontmatter index | every replica (core extractor) | entry | not synced; rebuilt from text |
| FTS index (notes) | every replica | entry | not synced |
| PDF text (per page) | **server**: pdfium via `pdfium-render`, in a sandboxed subprocess with timeout and rlimits, low priority | blob hash | `GET …/derived/pdf-text` (zstd-compressed JSON of pages), fetched at P4, indexed into local FTS |
| image display variant (long edge ≤1600 px, EXIF-oriented, JPEG/PNG-with-alpha) + thumb (≤256 px) | **server** (`image` crate plus zune decoders, in the same subprocess pool); **locally** for new pastes until the server's version arrives | blob hash | `…/derived/display`, `…/derived/thumb` |
| PDF first-page thumbnail | server (pdfium) | blob hash | `…/derived/pdf-thumb` |

Why the server (D6): each blob is extracted **once, ever** (it's content-addressed), not once per device. Mobile devices never spend battery on it, and they can show a 60 KB display variant instead of downloading a 12 MB photo just to show it inline. Extraction never blocks anything: it runs in a separate process with its own queue, and a PDF that crashes pdfium only marks `derived.status = error`. PDFs without a text layer produce empty text; OCR is future work. Optional: `libheif` in the image, so imported HEIC files get display variants that every platform can show. It's off unless you want it.

*Rejected:* PDF.js text extraction on each client (every device redoes the work and drains mobile batteries). Pure-Rust PDF text crates (quality is noticeably worse than pdfium on real-world PDFs).

---

## 9. Links, tags, frontmatter

### 9.1 Extraction (core, conformance-tested)

A single-pass scanner over the text skips fenced and indented code blocks, inline code, `%%comments%%` (Obsidian doesn't index links in comments), and maths. It emits:

- `WikiLink{embed, target, subpath (#Heading | #^block | #page=3&height=600), display (alias or size spec), range16}`
- `MdLink{embed, url (decoded, <> stripped), title, range16}`, plus bare external URLs are ignored
- `Tag{name, range}` following Obsidian's rules: `#` preceded by start or whitespace, a body of `[\p{L}\p{N}_/-]`, at least one non-digit, not a heading marker, not inside a URL. Plus frontmatter `tags`/`tag` (list or comma/space string).
- `Frontmatter`: a `---` block at byte 0, parsed with `saphyr` (a YAML 1.2 parser with no `unsafe` serde magic). It's stored as JSON in the index, and the text itself is never changed.

Extraction runs per note, off the main thread, debounced by 300 ms after edits. For a 1 MB note that's about 2 ms in Rust. The editor has its own incremental Lezer parse for display (§10).

### 9.2 Resolution (core `resolve`, mirrored by a tiny TS resolver for synchronous editor styling)

The input is a link target `T` and a source note path. Matching is case-insensitive on NFC-normalised strings. Only live, non-folder entries count.

1. External (`scheme:`) → not a vault link.
2. Markdown links: URL-decode, strip `<>`. Try the path relative to the source's folder, then vault-absolute, then fall through to rule 4.
3. `T` starts with `./` or `../` → resolve relative to the source's folder.
4. `T` contains `/` → exact vault path; then relative to the source's folder; then *suffix match* (entries whose path ends with `/T`).
5. Otherwise it's a basename match.
6. In every rule, `X` matches both `X` and `X.md` (the implied markdown extension). Other extensions must be written out.
7. If more than one candidate matches, the order is: the source's own folder, then the fewest path segments, then the shortest path, then lexicographic path order (deterministic).
8. Local `redirects` (§6.6) apply when nothing matches, or when the recorded pre-rename target differs.

Obsidian's exact tie-breaking isn't documented. Phase 1 includes a fixture set I'll check against real Obsidian behaviour (you can help by running it), and I'll document any deviations.

### 9.3 Index uses

- **Backlinks**: `links WHERE resolved = :id`, including embeds and PDFs.
- **Autocomplete**: an in-memory name index (§11.4).
- **Attachments panel**: `blob_refs`.
- **Tags panel**: counts from `tags`.
- **Rename**: server-side `links` (§6.6).

---

## 10. Editor

### 10.1 Structure

CodeMirror 6 + `@codemirror/lang-markdown` (GFM) + custom Lezer extensions (`WikiLink`, `Embed`, `InlineMath`, `BlockMath`, `Tag`, `Comment`, `Highlight`) + `y-codemirror.next`. The open note's `Y.Doc` lives on the main thread. Everything else lives in the worker or in Rust. The editor is created imperatively inside a thin Svelte component, and no Svelte state is updated from a CodeMirror transaction. (The one exception is a throttled word count and cursor position, sent through a plain callback.)

### 10.2 Live preview

- **Inline marks** (`**`, `_`, `[[`, `]]`, `$`, heading `#`s, link URLs) are hidden with `Decoration.replace`, except on lines that contain a selection range. This is a `ViewPlugin` that works only over `visibleRanges`, so its cost is O(viewport) and doesn't depend on document size.
- **Block widgets** (display maths, image and PDF embeds on their own line) come from a `StateField`, as CodeMirror requires for block decorations. The field is updated **incrementally**: map the existing `RangeSet` through the changes, then re-scan only the syntax-tree ranges touched by the changes. That keeps a 1 MB note O(change), not O(document).
- Syntax highlighting of large docs uses Lezer's incremental, time-sliced parsing (CodeMirror default).

### 10.3 Renderer registry

```ts
interface Renderer {
  id: string;                                   // 'math', 'image', 'pdf-embed', later 'mermaid', 'transclusion', 'recipe'
  match: { nodes?: string[]; embed?: (target: ResolvedTarget) => boolean };
  display: 'inline' | 'block';
  load: () => Promise<RendererImpl>;            // dynamic import → separate chunk
  estimateSize?(ctx): { width?: number; height: number } | null;   // reserve space, no layout shift
  cacheKey?(ctx): string;                       // e.g. math source string
}
interface RendererImpl { render(ctx, el: HTMLElement): void | (() => void) /* teardown */ }
```

The registry creates `WidgetType`s whose `eq()` compares the cache key, so rendered widgets are reused across transactions. Before its chunk has loaded, a renderer shows a placeholder sized by `estimateSize`. Unmatched embeds (`![[Note]]`, `![[Note#Heading]]`) get a link-style "transclusion" placeholder, which is where the transclusion renderer will plug in later. A renderer that throws is caught and shows the source with an error badge, so it never blanks the line.

### 10.4 Registry clients in the MVP

- **Maths** (§10.5).
- **Images**: `![[img.png]]`, `|300`, `|300x200`, `![alt](path)`, `![alt|300](path)`, remote `http(s)`. They render as `<img width height decoding="async" loading="lazy">`, sized from `blobs.width/height` (with orientation applied), and source the display variant (`/_blob/<hash>/display`). There are distinct placeholders for *unresolved* (no entry), *missing* (entry exists but the blob isn't on any known replica), *downloading* (with progress) and *remote offline*. SVG is rendered only through `<img>`, where scripts never execute, so it's safe by construction and needs no DOMPurify. Clicking opens the image viewer (a lazy chunk: pointer-event pan/zoom with CSS transforms, pinch on touch, loads `orig`).
- **PDF embeds**: `![[f.pdf]]`, `#page=3`, `#page=3&height=600`, `![](f.pdf)`. These show a static `pdf-thumb` image of the requested page (the server-rendered page 1, or a page rendered locally on demand). An `IntersectionObserver` with a 1-viewport margin turns it into a live PDF.js viewer. At most 2 live embedded viewers exist at once (LRU teardown), and each is torn down when it scrolls more than 2 viewports away.

### 10.5 Maths

Lezer inline parser `InlineMath` (Obsidian rules):

- Opening `$` must be followed by a non-space and not by another `$`.
- Closing `$` must be preceded by a non-space and not followed by a digit.
- `\$` is literal.
- The expression can't span a blank line.
- It isn't recognised inside code (Lezer precedence handles this).

`BlockMath`: `$$` on its own line through a closing `$$` line, or single-line `$$…$$`. An unterminated `$$` renders as plain text rather than swallowing the rest of the document. In live preview it's rendered unless the selection intersects the node.

KaTeX loads with `throwOnError: false`, `trust: false`, `maxSize`, `maxExpand`, `strict: 'ignore'`. It lives in a lazy chunk that's only loaded when the Lezer tree of the open note contains a maths node. Its fonts are bundled and cached by the service worker. Output is cached in an LRU `Map<displayMode+source, HTMLElement template>` of about 2000 entries.

**Caveat:** Obsidian uses MathJax. A few MathJax-only constructs (`\require`, some `\newcommand` scoping, rarer environments) will show a subtle inline error with the source instead. I think that's the right tradeoff (§19), but it is a compatibility gap.

### 10.6 Text fidelity (D11)

- The Y.Text is initialised with the exact decoded UTF-8 file, including the BOM and `\r`.
- CodeMirror is configured with `EditorState.lineSeparator.of("\n")`, so the CodeMirror document and the Y.Text are the same string. That makes position mapping exact, and y-codemirror stays correct.
- `\r` is removed from `highlightSpecialChars`, so it's invisible.
- A custom `insertNewline` inserts `\r\n` when the note's dominant ending is CRLF.
- Files that aren't valid UTF-8 are imported as blob-backed `markdown` entries, shown read-only with a notice, and exported byte-for-byte.

---

## 11. Frontend architecture

### 11.1 Shell

Svelte 5 (runes) + TS + Vite as a static SPA with no router library: a ~60-line hash router covers `#/note/<id>`, `#/settings` and `#/setup`.

```
App
├─ Sidebar (mode: pinned | shortcut | hover) ── FileTree (virtualised) · Tags · (later: search panel)
├─ Workspace  { layout: PaneNode }   PaneNode = Split{dir, children, sizes} | Pane{tabs: Tab[], active}
│    └─ Pane (MVP: exactly one, one tab) → View registry: 'markdown' | 'pdf' | 'image' | future kinds
├─ RightPanel (lazy): Backlinks · Outline(later)
├─ StatusBar: SyncIndicator
└─ Overlays (lazy): QuickSwitcher · CommandPalette · Settings · Import/Export · Attachments · ImageViewer
```

Workspace state is serialisable (`WorkspaceState` v1) and stored per device. Tabs and splits later will only extend the view layer. The data model doesn't change.

### 11.2 Backend interface

This is the only thing components talk to:

```ts
interface Backend {
  boot(): Promise<BootState>;                                 // last note + compact entries snapshot
  entries: EntryStore;                                        // fine-grained: subscribe(id), children(folderId), version counter
  intent(op: MetaIntent | MetaIntent[]): Promise<Result>;     // optimistic, returns prediction
  openDoc(id, slot): Promise<DocSession>;                     // {ydoc, dispose}; wires updates to persistence/sync
  blobs: { url(hash, variant): string; ingest(file, target): IngestHandle; progress: Readable<BlobProgress> };
  index: { backlinks(id), tags(), search(q, opts), resolve(target, srcId) };
  importer: { plan(source), run(plan, onProgress), cancel() };
  exporter: { zip(dest), toFolder?(path) };
  sync: Readable<SyncStatus>;
  device: DeviceSettings;
}
```

`WebBackend` is implemented over a dedicated worker (core WASM + Yjs + IDB + OPFS + lazy sqlite-wasm). `TauriBackend` is implemented over `invoke` and Channels, with the Rust side running the same core. Components can't tell which one they're using, and a `MemoryBackend` is used for component tests.

### 11.3 State

- `EntryStore` keeps flat typed maps (`Map<id, EntryMeta>` plus a per-folder sorted child-id array, rebuilt lazily when a folder's version changes). It exposes per-entity `$state` only where it's rendered.
- Note bodies and blobs never enter Svelte state.
- Search, backlinks and switcher results are id arrays rendered through a shared `VirtualList` (fixed row height, about 150 lines of code).

### 11.4 Quick switcher and autocomplete (<30 ms at 30k entries)

The main thread holds a name index. For each entry it stores the lower-cased NFC name, path and kind, plus a precomputed char bitmask for pre-filtering. Matching is fzf-style (subsequence with bonus scoring) over the pre-filtered candidates, and results are capped at 50. That's about 5 ms in JS at 30k entries. Images are only included when the query contains a `.` or matches `\.(png|jpe?g|gif|webp|svg|heic|bmp)$`, per the spec. Frontmatter aliases come from the index, once it has loaded.

### 11.5 Sidebar

One `Sidebar` component with three modes:

- **pinned**: a CSS grid column, with a width drag handle clamped to 180–600 px.
- **shortcut** and **hover**: `position: fixed`, moved only with `transform: translateX`, 150 ms, and `prefers-reduced-motion` turns animation off. It never reflows the editor.
- **Hover-reveal**: a fixed 8 px hot zone at the left edge. A `pointerenter` starts a 100 ms intent timer, which is cancelled on `pointerleave`, when `buttons !== 0`, or when a selection drag is active. Leaving the panel starts a 300 ms grace timer, which is cancelled on re-entry. It's disabled when `(hover: none)` or `(pointer: coarse)` matches.
- **Touch**: an off-canvas drawer with an edge-swipe (a 20 px start zone and velocity threshold), a scrim, swipe-back, and the Android back gesture (Tauri back-button event → close the drawer first).
- **Tree**: the WAI-ARIA tree pattern (`role=tree/treeitem`, `aria-expanded`, `aria-level`, roving tabindex) over a flat array of visible rows. Expanding a folder splices its cached sorted children into that array. The natural sort uses `Intl.Collator(undefined, {numeric: true, sensitivity: 'base'})`, with keys precomputed once per name. Folders come before files. The target is <16 ms for a 5,000-child folder, and it's benchmarked.
- **Reveal active note**: expands the ancestors and scrolls the row into view.
- The context menu, F2 and Delete call the **command registry**; they don't call handlers directly.

### 11.6 Commands, keybindings, tabs

- `commands.register({id, title, run(ctx), when?})`, and `keymap.bind('Mod-\\', 'sidebar.toggle')`. User overrides live in device settings.
- Tree operations (`entry.move(id, parentId, index?)`, `entry.rename`, `entry.trash`) are commands, so drag and drop later is just another caller.
- **One active tab** (D9): `navigator.locks.request('jess-active', {ifAvailable: true})`. A second tab shows a "use here" screen. Pressing it messages the current holder over `BroadcastChannel`, the holder flushes and releases, and the new tab takes over. Browsers without Web Locks use a `BroadcastChannel` handshake as a fallback.

### 11.7 Cold start

The critical path is: HTML → main JS (≤150 KB gz, excluding CodeMirror) → read the IDB `boot` record (one `get`) → construct the Y.Doc and CodeMirror → the note is visible. The worker (core WASM ~250 KB gz, Yjs, IDB) boots **in parallel**, and edits made before it's ready are buffered. The tree renders from the compact entries snapshot in `boot` (one structured-clone read of a packed binary array, about 30 ms at 30k entries) right after the note. On Tauri, `boot` is one Rust command that reads SQLite. The service worker precaches the app shell (a hand-written SW of about 150 lines with a build-generated manifest; Workbox isn't used).

### 11.8 Bundles

| chunk | loaded when | budget (gz) |
|---|---|---|
| main (Svelte runtime, shell, tree, Yjs, y-codemirror, lib0) | always | ≤150 KB (CI-enforced) |
| codemirror + lang-markdown + lezer | always (excluded from the budget, but tracked) | ~170 KB |
| worker + core.wasm | always, off-thread | ≤300 KB (tracked) |
| sqlite-wasm (index) | after first paint, idle | ~400 KB (tracked) |
| katex + fonts | the note has maths | lazy |
| pdfjs + worker | a PDF is opened or embedded | lazy |
| palette, settings, import/export, attachments, image viewer, QR | on demand | lazy |

### 11.9 Styling and accessibility

Plain CSS with custom properties (`--bg`, `--fg`, `--accent`, …). A `prefers-color-scheme` theme with a per-device override. Visible `:focus-visible` rings. Dialogs use `<dialog>`, with focus trapping and restore. Images show their alt text (the wikilink alias or markdown alt, with the file name as a fallback). Touch targets are ≥44 px under `(pointer: coarse)`.

---

## 12. Projection, import, export

### 12.1 Projection (core `projection`)

`project(entries, profile) -> Iterator<(rel_path, Source)>`, where `Source = Text(doc) | Blob(hash) | Dir`. It covers live entries only (trash is excluded by default, with an option to include it as `.trash/`). Paths are built from `parent_id` chains and the verbatim `name`s.

- **`exact` profile** (the default for the mirror, zip export, Linux folder export and the round-trip test): identity mapping. A name that the target filesystem can't store is the only thing mapped, reported as an error for folder export and never silently renamed.
- **`portable` profile** (optional, and used automatically when exporting to Android/iPadOS shared storage): replaces `<>:"/\|?*` and control characters with `_`, avoids Windows reserved names (`CON`, `NUL`, `COM1`…, with or without an extension) by appending `_`, strips trailing dots and spaces, normalises to NFC, and resolves case-insensitive collisions by suffixing ` (2)`, ` (3)`… in the order of sorted entry ids. That order is deterministic and stable across runs. Every mapping is listed in `EXPORT-REPORT.txt`.

The projection is shared by export, the server mirror, integrity-check and the tests.

### 12.2 Export

- The zip is **streamed**, using the `zip` crate in streaming-write mode natively, and a streaming writer (`client-zip`, about 2.6 KB, or core-wasm) feeding a `WritableStream` on the web. On Chromium it goes to `showSaveFilePicker`. Elsewhere the service worker streams a download. Entries are *stored*, not deflated, for media and PDFs, and deflated for `.md`. The zip entry's mtime is set from `modified_at`.
- Linux folder export writes files atomically into the chosen directory.
- Server: `GET /api/admin/export.zip` (streamed).
- Export is read-only against every store.

### 12.3 Import (core `import`, one planner for every source)

- **Sources**: a folder (Tauri Linux: native walk; web: `<input webkitdirectory>` giving a File list) or a zip (random access through `Read + Seek` over a `Blob` using `FileReaderSync` in the worker, or a file natively). Files are always streamed, never loaded whole.
- **Plan (a dry run is the same plan, not executed)**:
  1. Walk the source. Skip `.obsidian/`, `.git/`, `.trash/`, `.DS_Store`, `Thumbs.db`, `desktop.ini` and `__MACOSX/`, and record each skipped path. Reject zip entries with absolute paths or `..` (zip-slip) and flag suspicious compression ratios.
  2. Read *only* `attachmentFolderPath`, `newLinkFormat` and `useMarkdownLinks` from `.obsidian/app.json`, into the vault settings.
  3. Classify each file: `.md` → markdown (valid UTF-8 → text; otherwise a blob-backed note); `.pdf` → pdf, visible unless it's under the attachment folder (those are counted as "hidden by rule", and the dialog lets you flip the rule); everything else → media, hidden. `.excalidraw.md` counts as markdown.
  4. Compute folders, names, `created_at` and `modified_at` from file times where available.
  5. Resolve every link and embed with the core resolver against the *planned* vault to produce the report's unresolved list. Content is never modified.
  6. **Idempotency**: match each file to existing entries by exact path. Same path and same content (text bytes or blob hash) → skip. Same path and different content → a decision (ask, overwrite, keep both as `Name (imported).md`, or skip), defaulting to ask, with an "apply to all" option. Overwriting a note applies a minimal diff as a Yjs edit, so it merges with concurrent edits.
- **Execute** in batches of 250 notes as one op group each. Each Y.Doc is created with a single insert. Notes become usable as soon as their batch commits locally. Blobs are hashed and streamed into local storage, then queued for upload. The report covers notes, PDFs (and how many are hidden), images and other media, skipped items, unresolved links and embeds, and name collisions.
- **Throughput target**: 5,000 notes in well under a minute on desktop (expected <10 s for text). Blob work runs in parallel and in the background.
- **Very large vaults on web or mobile** (optional path): upload the zip *as a blob* over the resumable blob channel, then call `POST /api/admin/import {zip_hash}`. The server runs the **same core planner** natively and commits server-authored ops. This avoids needing the browser to hold 20 GB in OPFS just to push it to the server. Both paths produce identical ops and are covered by the same fixture test.
- **Round-trip test (CI)**: `tests/fixtures/vault/` → import (client path and server path) → export (`exact`) → compare byte-for-byte, recursively, ignoring skipped items. The fixture covers: unicode (NFC and NFD names), deep nesting, same-name notes and images in different folders, a 5 MB note, odd frontmatter, CRLF and mixed line endings, a BOM, non-UTF-8 `.md`, every attachment convention (same folder, `assets/`, `Note.assets/`, `../`, URL-encoded, `<angle>`), uppercase extensions, `#page=` and `&height=` embeds, orphan images, one image referenced by 50 notes, `.canvas`, `.excalidraw.md`, and empty folders.

---

## 13. Server mirror and git

- **Task isolation**: the mirror runs as its own tokio task, fed by a `watch` channel carrying `head_seq`. It reads through its **own read-only connection** (WAL snapshot isolation), so it never blocks the writer. Its state (`entry_id → path, content_hash, file_size`) lives in `/data/mirror-state.db`. Failures are logged, retried with backoff, and shown in the admin screen (`mirror: ok / lagging N s / error: …`).
- **Incremental**: debounced by 2 s. It computes the projection diff for entries changed since `mirror.last_seq` (plus descendants of moved folders), then writes the changed files, removes deleted ones, and prunes empty directories.
- **Atomic writes**: text is written to `.jess-tmp-<rand>`, fsynced, renamed over the target, and then the directory is fsynced. On startup, stray `.jess-tmp-*` files are removed, and entries whose on-disk size or mtime doesn't match the state are rewritten.
- **Blobs are hardlinked** from `/data/blobs` (same volume) to a temp name, then renamed into place, which is atomic. That costs no extra disk. Blob files are 0444 and the server runs as non-root, so nothing can open a mirror file for writing by accident. Git never modifies worktree files in place for commit or add (and the server never checks out). If hardlinking isn't possible (`EXDEV` or the filesystem doesn't support it), it tries a reflink (`FICLONE`), then falls back to copying, and warns in the admin screen. *Rejected:* symlinks (they break when the mirror is copied, and git stores the link rather than the file) and plain copies (they double disk use).
- **Markers**: `README-GENERATED.md` and `.jess-generated` at the root. `jess rebuild-mirror` wipes the mirror (except `.git`) and regenerates it.
- **Git** (the `git` binary, installed in the image):
  - A commit happens after 60 s of quiet (`JESS_GIT_COMMIT_INTERVAL`), and at least every 10 minutes while changes are pending.
  - The message summarises the changes: `Jess: 3 edited, 1 added, 1 renamed (Old → New), 2 attachments`, with a file list in the body.
  - `git gc --auto` runs after commits, and a full `git gc` runs weekly.
  - The generated `.gitignore` excludes every non-`.md` file unless `JESS_GIT_INCLUDE_ATTACHMENTS=true`.
  - **Push**: on first run the server runs `ssh-keygen -t ed25519` into `/data/git/deploy_key`, and the admin screen shows the public key. Pushes use `GIT_SSH_COMMAND="ssh -i … -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=/data/git/known_hosts"`. A push failure is non-fatal, retries with backoff, and shows in the admin screen.
  - Why the `git` binary over libgit2 or gitoxide: it's battle-tested, `gc` and repacking are built in, SSH push works with no libssh2 quirks, LFS is available if wanted, and it's easy to debug (`docker exec … git log`). It adds about 25 MB to the image, which is a fine trade. libgit2 has no `gc` and a fussier SSH setup. gitoxide's push support is still incomplete.
- **Git LFS evaluation**: LFS would keep attachment history out of the pack, but it *still* stores a second copy under `.git/lfs/objects`, and it needs a remote that supports LFS (GitHub charges for LFS bandwidth and storage). Recommendation: keep attachments out of git by default. If `JESS_GIT_INCLUDE_ATTACHMENTS=true`, use plain git unless `JESS_GIT_LFS=true` is also set, in which case `.gitattributes` tracks `*.pdf`, images and similar with LFS. Either way, DEPLOY.md says clearly that including attachments roughly **triples** attachment storage (blobs, mirror hardlinks at zero cost, git objects).

---

## 14. Auth and threat model

### 14.1 Auth

- **Setup**: if no account exists and `JESS_ADMIN_PASSWORD` is unset, the server logs a one-time `SETUP CODE: XXXX-XXXX` (D10). The setup screen needs that code plus the new password. If `JESS_ADMIN_PASSWORD` is set, the account is created from it on startup, and changing the env var later doesn't overwrite a changed password.
- **Password**: argon2id (m=64 MiB, t=3). `jess reset-password` works from the container shell.
- **Login** returns a random 256-bit device token. The server stores only its SHA-256. The token is sent as `Authorization: Bearer`.
- **Rate limit**: login attempts are limited to 5 per minute per IP, and a global exponential lockout starts after 20 failures an hour. Responses are constant-time, and the rate limit sits in front of the argon2 check.
- **Pairing**: a logged-in device creates a one-time code (10 minutes, single use, 128-bit). It's shown as a QR code or link `https://host/#pair=<code>`, and the Tauri apps accept a pasted link or a scan. The code is exchanged for a device token.
- **Devices** are listed in settings (name, last seen) and can be revoked. A revoked token is disconnected immediately.

### 14.2 Threat model

| asset / threat | mitigation |
|---|---|
| Network attacker | TLS at Traefik (assumed). HSTS set by the server. No tokens in URLs. |
| Internet scanners / brute force | rate limits, argon2id, setup code (D10), and no account enumeration because there's a single account |
| Stolen or lost device | revoke its token. Local data isn't encrypted at rest; that relies on OS or device encryption, and is stated in the docs. |
| XSS via note content | CodeMirror renders text as DOM text. Widgets are built with DOM APIs, never `innerHTML`, except KaTeX output (`trust:false`). **Raw HTML in markdown is shown as source in the MVP**, and a later HTML renderer would use DOMPurify. SVG only renders through `<img>`. CSP: `default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; img-src 'self' blob: data: https:; connect-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'`. |
| Malicious PDF | PDF.js with `isEvalSupported:false` and scripting off. On the server, pdfium runs in a subprocess with a timeout, memory rlimit and no network. |
| Malicious import zip | zip-slip rejection, a ratio limit and a total size limit, and streaming (no zip bombs in memory) |
| DoS | a body limit on sync frames (1 MiB), chunk size limit, max file size, per-device concurrent upload limit |
| SSRF | none: the server never fetches user URLs, and remote images aren't proxied. (Privacy note: remote images reveal your IP to their host. A per-vault "block remote images" option is planned for later.) |
| Git remote exposure | the mirror is plaintext notes. A deploy key scoped to one repo is recommended, and DEPLOY.md warns to use a private repo. |
| Server disk compromise | out of scope (end-to-end encryption is a non-goal). Rely on host disk encryption. |
| Snapshot / backup leakage | snapshots live in `/data` alongside the data. The same trust boundary applies. |

---

## 15. Server operations

- **CLI**: a single `jess` binary with `serve` (the default), `integrity-check [--hashes] [--mirror]`, `rebuild-mirror`, `snapshot`, `reset-password`, and `gc --dry-run`.
- **Env**: `PORT` (8080), `JESS_DATA_DIR` (`/data`), `JESS_ADMIN_PASSWORD`, `JESS_MAX_UPLOAD_MB` (2048), `JESS_MIRROR_ENABLED` (true), `JESS_GIT_ENABLED` (true), `JESS_GIT_REMOTE`, `JESS_GIT_COMMIT_INTERVAL` (60), `JESS_GIT_INCLUDE_ATTACHMENTS` (false), `JESS_GIT_LFS` (false), `JESS_SNAPSHOT_INTERVAL_HOURS` (6), `JESS_SNAPSHOT_RETENTION_DAYS` (14), `JESS_BLOB_RETENTION_DAYS` (30, forced ≥ snapshot retention + 7), `JESS_TRASH_RETENTION_DAYS` (30), `RUST_LOG`.
- **Snapshots**: the SQLite online backup API writes `/data/snapshots/jess-<UTC>.db.zst`. Retention keeps everything from the last 48 h, then one per day up to the retention limit. The admin screen can download the latest one.
- **`integrity-check`**:
  - `PRAGMA integrity_check`, and orphan and dangling-parent checks.
  - The tree is acyclic, and live names are unique.
  - Every doc decodes in yrs, and its merged state equals the log.
  - The `links` index is consistent with the text.
  - Every referenced blob exists with the right size, and with `--hashes` its SHA-256 is re-verified.
  - There are no stray temp files.
  - The mirror equals the projection (`--mirror`).
  - It exits non-zero on any failure.
- **Graceful shutdown** (SIGTERM from Coolify): stop accepting connections, finish in-flight transactions and chunk writes (fsync), flush the mirror queue (bounded to 10 s), `wal_checkpoint(TRUNCATE)`, then exit. Coolify's default stop timeout is 10 s, and DEPLOY.md suggests raising it to 30 s.
- **`/healthz`**: 200 when the database is writable (it runs a trivial read on the writer thread's queue). The mirror and git state are reported by `/api/admin/status`, not by health, so a git failure never restarts the container.
- **Docker**: multi-stage build:
  1. `node:22` builds `ui/`.
  2. `rust:1.93` with cargo-chef caching builds `jess`.
  3. `debian:bookworm-slim` with `git`, `openssh-client`, `ca-certificates`, `tini` and the pdfium shared library (pinned bblanchon/pdfium-binaries release, checksum verified).

  It runs as a non-root user with uid 1000, exposes one port and one `/data` volume, and uses `HEALTHCHECK` on `/healthz`. The expected image size is about 120 MB.

---

## 16. Platform notes

- **Tauri 2 Linux**: WebKitGTK. Everything heavy is native, so the WebView only needs ES2020 plus CSS custom properties. WebKitGTK support is checked for `OffscreenCanvas` (local thumbnails fall back to Rust anyway), `ResizeObserver` and `IntersectionObserver`. Packaged as AppImage and deb.
- **Android**: the system WebView (target: Chromium 100+ in CI, with a documented minimum). The Rust core, SQLite and file blobs are native. The photo picker uses the Tauri dialog/file plugins. The keyboard-aware editor uses `visualViewport` resize with `interactive-widget=resizes-content`. Back gesture handling is described in §11.5.
- **iPadOS** (not scheduled: possible later, see §21): WKWebView. HEIC conversion on paste uses WebKit's decode through a canvas, and a Swift plugin for ImageIO is optional later. Pencil input works as a pointer (no drawing). Hardware keyboard shortcuts go through the keybinding registry (Cmd). **What you'll need on a Mac** (details in docs/MAC.md, phase 7): Xcode, an Apple ID with a paid Developer Program membership to install on a real iPad for more than 7 days, `rustup target add aarch64-apple-ios`, `pnpm tauri ios init`, `pnpm tauri ios build`, signing set in Xcode. The iOS target will be kept compiling in config from phase 5 onwards.
- **Cold start on Android** is dominated by WebView initialisation (about 150–300 ms on mid-range devices). The <500 ms target is measured from `Activity.onCreate` to the `note-visible` mark. That's the riskiest performance target, and phase 8 may need a native splash plus pre-warming.

---

## 17. Testing strategy

1. **Deterministic simulation** (`server/tests/sim`, Rust):
   - The real `jess-server` apply loop runs on in-memory SQLite, with N `core` sync clients (native backends using yrs) and a simulated network (seeded RNG: drop, duplicate, reorder and delay messages; partitions; offline periods), plus crash points. A crash discards uncommitted local transactions or server transactions mid-batch.
   - Each client has a skewed clock (±days).
   - Workload: random meta ops (create, rename, move, trash, restore, purge, visibility, `SetBlob`), random text edits including link insertions and edits inside links, blob ingest, and interrupted, duplicated or resumed chunk uploads and downloads.
   - After it quiesces, the test asserts:
     - Every replica is identical (entries plus doc text).
     - No acknowledged op is lost.
     - No live duplicates.
     - No resurrection, except the documented "Recovered" rule.
     - Every blob referenced by a live entry is present on the server, or local on its creator.
     - No blob was evicted before confirmation.
     - Invariant R holds for links that weren't concurrently edited.
   - 10,000 seeds run on every CI run and a million run nightly. A failing seed is printed with a minimised trace.
2. **Property tests** (`proptest`): rename/move/trash/edit races, link rewrite racing concurrent edits (notes and PDFs), and visibility and path races for attachments. Also: the resolver is consistent with the rewrite formatter (formatting a link for a target always resolves back to that target).
3. **Crash safety** (process level): spawn `jess serve`, drive load, `SIGKILL` at random points (including mid-chunk and mid-mirror write), restart, and run `integrity-check --hashes --mirror`. It must pass, with no partial blobs and no partial mirror files.
4. **Yjs ↔ yrs compatibility**: a fixture suite of update exchanges (JS generating and Rust applying, and the reverse) with UTF-16 offsets, emoji and CRLF.
5. **Conformance fixtures** (`core/fixtures/*.json`): links, tags, maths, embed specs (`|300x200`, `#page=3&height=600`), resolution, sanitisation. Run by `cargo test` and by Vitest against the TS Lezer grammar and TS resolver.
6. **Vitest**: storage and sync layer (against `MemoryBackend` and a fake worker), the renderer registry, maths edge cases (currency, `$$` adjacency, escapes, code, unterminated blocks).
7. **Component tests** (Vitest + @testing-library/svelte, fake timers): all three sidebar modes, hover timing (100 ms intent, 300 ms grace, suppressed with a button held), keyboard tree navigation, media hidden and PDF hide/show, and the quick switcher.
8. **Round-trip and mirror**: the fixture vault round trip (§12.3). Mirror == projection after random op sequences (driven by the sim) including attachment add, rename and delete. Collision cases. Mirror lag never affects sync latency (a test with the mirror artificially blocked still meets the sync latency bound).
9. **Playwright** (Chromium and WebKit): setup, import of the fixture vault, create/edit/search, maths, pasting an image (inline and not in the tree), a standalone PDF, an embedded PDF, sidebar modes, and two-browser-context sync.

---

## 18. Benchmark plan

The seeded generator is `tools/vaultgen --notes 10000 --attachments 20000 --pdfs 300 --large-pdf 100MB/500p --seed N`. It produces realistic text (links, tags, maths, frontmatter), deep folders, and images of assorted sizes.

| metric | harness | target (fails CI if exceeded, with 10% noise tolerance) |
|---|---|---|
| cold start → last note visible | Playwright + CDP, `performance.mark('note-visible')`, warm SW and IDB, 10k vault | <300 ms desktop CI runner (4× CPU throttle profile tracked separately as a mid-range proxy) |
| open note | mark from command to first CodeMirror paint; a note with 50 images (no layout shift: CLS = 0 via LayoutShift entries) | <50 ms |
| switcher / autocomplete / search | query → results rendered | <30 ms at 10k notes and 20k attachments |
| typing latency | Event Timing `interactionId` durations plus Long Animation Frames while typing 200 chars in: a 1 MB note, a maths-heavy note, and a note with 30 images and 3 PDFs | p99 <16 ms, 0 LoAFs over 50 ms |
| tree expand | folder with 5,000 children | <16 ms |
| PDF first page | 5 MB PDF local → first canvas | <300 ms |
| large PDF | 100 MB / 500 pages: bytes fetched before the first page, and peak JS+canvas memory (CDP) | first page <1 s, fetched <10% of file, memory cap documented (target <300 MB with `maxCanvasPixels = 4 MP` on mobile) |
| sync latency | two headless clients against a local server, edit → remote apply | p50 <150 ms, p99 <1 s |
| catch-up | 200 pending remote changes, foreground → applied | <500 ms |
| import | 5k notes → all usable | <30 s |
| bundle size | `vite build` + gzip per chunk vs `bench/budgets.json` | main ≤150 KB gz |
| server | sim-style load: 5 clients typing | apply+commit p99 <20 ms |

Real mid-range Android numbers come from a documented manual run (a Tauri debug build plus Chrome remote debugging, using the same harness). CI can't reliably provide a device, so the emulator with throttling acts as a regression guard only. CI provider: I'm assuming GitHub Actions (tell me if you'd rather use something else).

---

## 19. Alternatives considered

| choice | chosen | rejected, and why |
|---|---|---|
| Text CRDT | **Yjs / yrs** | **Automerge**: larger WASM (~1 MB), historically slower text, and a weaker CodeMirror binding. **Loro**: very fast, and has a native movable-tree CRDT, which is tempting for metadata, but it's younger, the format has changed recently, its CodeMirror binding is less mature, and the WASM is over 1 MB. Yjs↔yrs binary compatibility gives one format on the server and clients. |
| Metadata | **server-ordered log + HLC LWW registers** (D1) | **Movable-tree CRDT** (Kleppmann et al.): needed for peer-to-peer, but we always have a server. Server ordering makes validation (cycles, collisions, link rewrite) straightforward, deterministic imperative code instead of undo/redo replay on every replica. |
| Sync substrate | **custom log + cursor** | **Plain-file two-way sync** (Syncthing or Obsidian-Sync style): conflict files, no merging, renames look like delete plus create, and it's lossy under races. **CouchDB/PouchDB**: document-level conflicts and a heavy client. |
| Shared logic | **Rust core → native + WASM** (D5) | **TypeScript everywhere**: the server and native clients would need a second implementation, and the simulation would test different code from what ships. **Separate TS client + Rust server**: two sync clients to keep in agreement forever. |
| Frontend | **Svelte 5 SPA** | **React**: bigger runtime and VDOM reconciliation on typing-adjacent state. **SolidJS**: comparable performance, smaller ecosystem, and you specified Svelte. **SvelteKit**: its router, SSR and adapters add nothing to an offline SPA (a static adapter works but means configuration for no benefit). **Astro/Next**: built for content sites and SSR. |
| Maths | **KaTeX** | **MathJax** (Obsidian's engine): better coverage, but 3–10× slower and much larger, and async rendering causes reflow. Covered by the compatibility caveat in §10.5. |
| PDF | **PDF.js** everywhere | **Native viewers** (PDFKit, Android PdfRenderer, poppler): three implementations, can't embed inline in a WebView consistently, no uniform text layer or find. |
| Web search index | **sqlite-wasm FTS5** (lazy, OPFS) | **MiniSearch/FlexSearch**: an in-memory index of 50 MB+ of text is too much for mobile, and search semantics would differ from native FTS5. |
| DB access | **rusqlite** (bundled, FTS5) | **sqlx**: compile-time macros, async overhead that SQLite doesn't need, and a heavier build. |
| Wire format | **CBOR (minicbor)** | **JSON**: bulky for binary Yjs updates. **protobuf/prost**: codegen toolchain. **postcard**: not self-describing, which makes schema evolution harder. |
| Blob auth | **SW + bearer / custom scheme** (D7) | signed URLs, cookies (§7.7) |
| Git | **git binary** | libgit2, gitoxide (§13) |
| Mirror blobs | **hardlinks** | copies, symlinks (§13) |
| PDF text | **server pdfium** (D6) | per-client PDF.js, pure-Rust crates (§8) |
| Service worker | **hand-written (~150 lines)** | **Workbox**: more code than we need |

### Heavy dependencies (justified)

- `yrs`: required for server-side merging and rewriting.
- `pdfium` (about 6 MB shared library, server only): the quality of text extraction and thumbnails.
- `pdfjs-dist`: the only realistic cross-platform PDF viewer (lazy-loaded).
- `katex`: lazy-loaded.
- `sqlite-wasm`: lazy-loaded (web only).
- `image` plus zune decoders: server only.

Everything else is small: axum, tokio, rusqlite, sha2, minicbor, argon2, saphyr, zip, uuid, imagesize; and on the TS side codemirror, yjs, y-codemirror.next, lib0, client-zip.

---

## 20. Risks and how I'll retire them early

1. **Yjs ↔ yrs subtle incompatibilities** (UTF-16 offsets, update v1 edge cases). Retire it with the compatibility fixture suite in the first week of phase 1.
2. **Server-side rewrite correctness** (Invariant R with markdown-link encodings and relative paths). Retire it with property tests, the resolver/formatter round-trip property, and the fixture vault.
3. **Android cold start** (WebView initialisation). Measure early in phase 6; mitigate with pre-warming.
4. **OPFS in service workers on Safari**: if it isn't available, the SW path reads from IDB chunks (a slower but working fallback). Verify in phase 4.
5. **Obsidian resolution tie-breaks**: verify against real Obsidian with your help (§9.2).
6. **KaTeX vs MathJax differences in your notes**: phase 3 includes a scan of your vault's maths that reports expressions KaTeX rejects, so we know the real gap before committing.

---

## 21. Phase plan (unchanged from the brief, with notes)

1. Core + server + sync protocol + blob store/channel + simulation suite (no UI). This is also where the conformance fixtures and the Yjs↔yrs suite start.
2. Projection, import/export engine, mirror + git, round-trip and mirror tests.
3. Web app shell, sidebar (3 modes), editor (wikilinks, maths, live preview), backlinks, tags, search, import/export UI, sync status, Dockerfile, DEPLOY.md.
4. Images and PDFs in the UI, PDF text search, attachments manager, blob progress.
5. Tauri Linux (native SQLite, file blobs, native folder import/export).
6. Android.
7. Performance pass against §18, then polish.

**Plan change (owner, 2026-09-30):** the iPadOS phase (originally 7, "iPadOS preparation +
docs/MAC.md") is dropped from the plan and kept as a possibility for later (see "Possible later"
in `docs/PHASES.md`). The web app still supports iPad Safari. Performance + polish becomes
phase 7. Nothing already built changes: the shared mobile entry point stays for Android.

Each phase ends with tests passing, a commit pushed to `origin/main` (github.com/dochmccrum/jess-notes, private), and a short summary in `docs/PHASES.md`.

---

## 22. Implementation notes and refinements (recorded during phase 1)

These are refinements found while building and simulating phase 1. None changes a D1–D11 decision;
each tightens a rule the simulation showed was underspecified.

1. **Dedupe is by exact `(replica_id, op_id)`**, not `op_id ≤ last_op_id` (§5.2 step 1). Pushes can
   arrive reordered (HTTP fallback, retries), and a high-water mark would silently drop an earlier op
   that arrives late. `op_results` is the dedupe set; when it is pruned (30 days) the replica's
   `pruned_upto` watermark answers `Duplicate(0)` for ops at or below it. A replayed op gets its
   original outcome, so a rejection stays a rejection (the client quarantines it again, never drops it).
2. **Purge keeps the doc's merged update log** in `doc_purged.state` (not only its state vector), and
   "not covered by the purged state" is an exact inserted-ID-set containment check. The server's yrs
   doc can hold *pending* structs (a reordered update waiting for its dependency); a state vector
   overstates coverage in that case and `encode_state` drops pending structs, both of which lost text
   in the simulation. A recovered note's new first row is `merge(purged_state, update)`. A doc update
   fully covered by the purged state is acknowledged `Duplicate(0)` and carries nothing to keep.
   (Privacy note: purged text therefore remains in `jess.db` as a tombstone payload.)
3. **Purged docs are cleaned up immediately inside the transaction** (not at commit time), so a
   later op in the same push can recover them; groups use a SQL `SAVEPOINT` so a rejected group
   can't leave purged rows behind.
4. **Resolver tie-break adds exact case** (§9.2 rule 7): own folder, then a candidate whose path
   matches the written target case-sensitively (NFC), then fewest segments, shortest path,
   lexicographic. D8 allows `note.md` and `Note.md` side by side; without this, no link text could
   point at one of them and Invariant R could not be satisfied. This is also what Obsidian does on a
   case-sensitive filesystem.
5. **Renames of trashed entries are recorded** in `rename_history` and run the Invariant R pass, so
   links inside trashed notes keep their meaning if they're restored.
6. **`trashed.at` is the applier's wall time** (server time on the server), not the op's HLC wall, so
   a device with a skewed clock can't get its trash purged early by the retention task.
7. **`Changes` pages are cut at commit boundaries** using a small `commits(first_seq, last_seq)` table,
   so a rename and its rewrites are never split across pages. `rename_history` stores full old/new
   vault paths (plus `is_folder`) rather than parent/name, which the stale-link rule needs.
8. **Client persistence is one ordered key-value store** (`core::kv`: native SQLite table, web
   IndexedDB store). The sans-IO client returns `writes` that the host commits atomically before
   sending anything. The client keeps only an index of confirmed doc rows in memory; bytes stay in the
   store. A purge row deletes local doc rows only up to its own seq (later rows belong to a recovery),
   and an ack never overwrites a doc row already received for the same seq.
9. **Blobs:** every new reference (ingest) re-verifies with the server even if the device once saw the
   blob confirmed; a blob row with `present = 0` (GC) sends a confirmed local copy back to the upload
   queue. GC deletes files on the writer thread after re-checking the blob is still absent and has no
   upload session, so a concurrent re-upload is never deleted.
10. **Dependencies:** `yrs` is pinned to 0.26 (0.27+ needs a newer rustc than the 1.93 toolchain).
    The `uuid` crate was dropped (UUIDv7 is 8 lines; avoids a randomness backend in WASM). The HTTP
    client in tests is `ureq`, the WebSocket client `tokio-tungstenite` (dev-dependencies only).
11. **Rewrite formatting keeps a leading `/`** on vault-absolute links, and markdown links that
    resolved relative to their folder stay relative. Suffix/basename fallbacks mean many moves need
    no rewrite at all (e.g. `[[Proj/Plan]]` still resolves after `Proj/` moves into `Archive/`); the
    server only rewrites links whose resolution would actually change.

### Phase 2 notes

12. **Export zips are written by a small streaming ZIP64 writer in core** (`core::zipstream`:
    data descriptors, UTF-8 names, stored or deflated entries, extended-timestamp extra, ZIP64 when an
    entry or offset needs it). The `zip` crate is used only to *read* imports. This streams a multi-GB
    vault to a socket or file without temp files or seeking.
13. **Git excludes live in `.git/info/exclude`** (and LFS rules in `.git/info/attributes`) rather than a
    generated `.gitignore` in the mirror root, so no generated file can collide with a vault file of the
    same name and the mirror stays exactly equal to the projection.
14. **The mirror and exports read through a `Snapshot`** (one read transaction on their own connection):
    a consistent state that never blocks the writer. The mirror recomputes the whole projection in
    memory each run (cheap) but only rewrites files whose entry, path or content changed (docs with
    rows newer than its `last_seq`; text is skipped when its SHA-256 is unchanged).
15. **Server-side import runs each batch as its own writer job** (`ServerSink` over the `Writer`), so a
    large import never blocks sync for long. Import ids are hash-derived per run (74 random bits kept).
16. **Import details:** a single top-level folder in a zip is stripped (vault zips are usually
    `Vault/…`); `\` separators are normalised; zip entries that are absolute or contain `..` are
    skipped and reported; entries over 16 MiB with a compression ratio above 1000 are skipped as
    suspicious; `._*` AppleDouble files are skipped like `.DS_Store`. `attachmentFolderPath` values
    `/`, `./` and empty mean "no dedicated folder" (nothing hidden); `./sub` hides PDFs in any folder
    named `sub`; anything else is a fixed vault folder. Case-insensitive name collisions are listed in
    the report (they matter only for the portable profile). If the vault record isn't known yet (a
    client that hasn't synced), applying the attachment settings is a warning, not a failure.
17. **GC never deletes a blob file modified in the last hour**, closing a race with server-side import
    (file installed, row not yet recorded).
18. **Web import executes in TypeScript; planning stays in core.** The worker asks core (WASM) for
    the plan (paths, kinds, visibility, conflicts, settings) and then issues ordinary meta ops and
    Yjs updates in batches of 250. Keeping execution in the worker avoids marshalling every file
    body across the WASM boundary twice; the semantics that matter (what is imported where, and how
    conflicts resolve) are still core's. Text is decoded with `ignoreBOM: true` so BOMs survive
    (caught by the in-browser byte-for-byte round-trip e2e test).
19. **Worker calls wait for `init`.** The UI renders the tree from the IndexedDB boot record before
    the worker is ready; any call (opening a note, typing) that arrives first is queued behind
    `init`, in arrival order. Measured on the dev machine: reload → last note visible ≈ 150 ms;
    opening a note 7–23 ms.
20. **App keybindings run in the capture phase**, before the editor's own keymap, as in Obsidian:
    a user-bound app shortcut always wins. Commands that act on "the current item" use the tree's
    selection only while the tree has keyboard focus, otherwise the open note.
21. **Link autocomplete applies on Enter immediately** (`interactionDelay: 0`), matching Obsidian.
22. **Rarely used panels (palette, settings, import/export, trash) are separate chunks,
    prefetched when the app is idle**, so opening one is instant and keystrokes typed right after
    the shortcut are not lost.
23. **`online`/`offline` events act immediately:** `offline` drops the socket (status shows
    Offline at once, instead of after the heartbeat timeout); `online` replaces a socket that is
    stuck connecting and reconnects with the backoff reset.
24. **Erase this device's local copy** (Settings) deletes IndexedDB and OPFS on the next load,
    before anything opens them, keeping only device preferences. It warns with the count of
    changes not yet on the server. It is also the documented recovery path after a server snapshot
    restore, when devices refuse to sync with a server that is behind them (DEPLOY.md §6).
25. **Login rate limit is configurable** (`JESS_LOGIN_RATE_PER_MINUTE`, default 5 as specified);
    the e2e suite raises it because every browser context signs in as a new device.
26. **`jess health`** is a tiny HTTP check used by the image's `HEALTHCHECK` (the runtime image
    has no curl). The Dockerfile takes `--build-arg REGISTRY=…` for a Docker Hub mirror.
27. **pdfium is added to the image in phase 4** together with the derivation subprocess that uses
    it; the phase 3 image has no PDF processing.
28. **Service worker precache list** is injected into `sw.js` after the build
    (`scripts/sw-manifest.mjs`); navigations are network-first with a 2.5 s timeout and fall back
    to the cached shell, hashed assets are cache-first, `/api/*` is never intercepted.
29. **Derivation runs as a subprocess of the same binary** (`jess derive image|pdf <in> <outdir>`),
    one job at a time, with `RLIMIT_AS` 2 GiB, `RLIMIT_CPU` 120 s, `RLIMIT_FSIZE` 256 MiB, no core
    dumps, `nice 10` and a 120 s wall-clock timeout. Results go to
    `/data/derived/<kind>/<hex>[.png]` and a `derived` row (`ok` / `error` / `unavailable` when
    pdfium is missing; `unavailable` rows are retried once pdfium appears). Bumping a kind's version
    re-derives everything. *Gap:* the subprocess is not network-isolated (that needs namespaces and
    privileges the container doesn't have); it never opens sockets itself.
30. **pdfium comes from the `pypdfium2` 5.13.0 wheel on PyPI** (the pdfium-binaries
    `chromium/7999` build, the same upstream as bblanchon's releases), pinned by SHA-256 per
    architecture in the Dockerfile. PyPI files are immutable and their hashes are published, which
    made pinning possible from this environment. `JESS_PDFIUM_LIB` overrides the library path.
31. **Clients keep blob facts** (size, mime, oriented dimensions) from the server's blob rows, in
    the KV store under a new `f` prefix; the server's row is authoritative and local ingest fills
    gaps (so a paste renders at its size immediately). Entries sent to the UI carry `blobInfo`.
    When bytes reach the server before any entry describes them (server-side import, or an upload
    that finishes first), the first entry that does fills the missing facts on the row, with a new
    seq so clients learn them. Known facts are never overwritten.
32. **`pdf-text` is stored zstd-compressed but served as JSON** (`{"pages": [...]}`), gzip-encoded
    when accepted: browsers can't decode zstd everywhere.
33. **PDF.js uses its legacy build**: the modern 5.x build relies on very new JS
    (`Map.prototype.getOrInsertComputed`) missing from current WebViews and WebKitGTK. Range
    requests are 1 MiB; the worker keeps the last 4 chunk records in memory so PDF.js's small reads
    don't re-read IndexedDB. Measured (dev machine, Chromium, bytes local): a 5 MB PDF whose first
    page holds a 5 MB image shows page 1 in 250–270 ms; a note with 50 images opens in 16–21 ms.
34. **Web blob storage is IndexedDB chunk records only** in this phase (the OPFS SyncAccessHandle
    path from §7.5 is deferred to the phase 7 performance pass); the service worker reads the same
    records. Until the server's display variant exists, `/_blob/…/display` falls back to the
    original bytes (which are local on the pasting device) instead of generating a thumbnail locally.
35. **Offline attachments policy** lives in the worker: `everything` queues every missing blob at
    P3; `on-demand` evicts to a 1 GB LRU cap (never unconfirmed blobs — core enforces it). A
    `QuotaExceededError` while storing downloads switches the device to on-demand, evicts, and
    shows a banner.
36. **Live preview details:** an embed becomes a block widget only when it is the whole of its
    paragraph (otherwise inline); rendered widgets other than maths keep their own events (viewer,
    buttons, scrolling) instead of turning back into source on click; a `refreshLinks` effect
    re-resolves links and embeds when entries change (a pasted file's entry appears, facts arrive).
37. **Attachment names:** clipboard files named `image.*` (screenshots) get Obsidian's
    `Pasted image YYYYMMDDHHmmss.ext`; dropped or picked files keep their names; collisions in the
    target folder get ` 1`, ` 2`… HEIC/HEIF on Apple devices is converted to JPEG (0.92) first.
38. **PDF memory cap:** at most 12 rendered pages per viewer (6 on touch devices), device-pixel
    ratio capped at 2 on touch devices; pages far from the current one are released first. At most
    2 live embedded viewers exist; embeds show the server's page-1 thumbnail until live.
39. **The search index has a schema version** (now 2: a `page` column and a `pdfs` table); a
    mismatch drops and rebuilds it. Backlinks, tags and search results refresh on an `indexed`
    event, and answer only after pending edits are indexed.

### Phase 5 (Tauri Linux)

40. **The native backend is its own crate, `apps/native` (`jess-native`), with no Tauri
    dependency.** It does what the web's sync worker does (core sync client, `app.db` KV store,
    `index.db`, file blobs, transport, import/export) and emits the same `{ev: …}` events, so
    `TauriBackend` is a thin IPC mapping and the whole backend is tested headlessly against an
    in-process server. To allow that, the server's `serve` moved into the library
    (`jess_server::serve`); `main` only parses the CLI.
41. **Native transport dependencies:** `tokio-tungstenite` (rustls, webpki roots) for the sync
    WebSocket and `ureq` for HTTP (long-poll fallback, blob chunks, auth). Both were already in
    the tree as test dependencies (item 10); on native they are real dependencies. Auth requests
    go through Rust, so the app needs no CORS on the server.
42. **IPC shape:** JSON commands mirror `ui/src/backend/tauri.ts`; doc updates and blob bytes
    travel as raw binary IPC bodies (length-prefixed when batched), and events arrive over one
    `Channel`. `init` returns a snapshot; events that race ahead of the reply are held and
    replayed after it, so a stale snapshot never overwrites a newer status.
43. **Blocking work runs off the IPC and GUI threads** (`spawn_blocking`), and `Native` is opened
    inside the Tauri async runtime, keeping a runtime handle for tasks started later from other
    threads.
44. **Erase this device on native** marks the data directory and deletes it at the next launch,
    before any store is opened (the stores are open while the app runs).
45. **No asset compression in the Tauri bundle:** decompressing every script at launch cost
    cold-start time; the binary is a few MB larger instead.
46. **Blob transfer refinements found in this phase** (apply to web too): a download the server
    can't serve yet is *parked* until the blob's row says it's present or the client reconnects
    (was a hot retry loop); a blob whose size isn't known yet isn't queued until the server's
    row gives it; export skips an unavailable attachment and lists it in `EXPORT-REPORT.txt`
    instead of failing.
47. **Linux packages:** `.deb` (~10 MB) and AppImage (~116 MB; it bundles WebKitGTK). The CI job
    `linux-app` builds both and runs the WebDriver smoke test (`apps/tauri/e2e/smoke.mjs`) under
    Xvfb. The Android build of the same crate (the `mobile_entry_point` is in place) needs the
    Android NDK, so it is first compiled in phase 6.
