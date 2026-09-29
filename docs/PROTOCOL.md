# Jess sync protocol (v1)

Written from the types in `core/src/proto.rs`, `core/src/ops.rs` and `core/src/model.rs`; the golden
encodings at the end are checked by `core/tests/protocol_doc.rs`, so this file can't silently drift.
Design background: DESIGN.md §5–§7.

## Encoding

- WebSocket **binary** frames (`GET /api/sync`) and HTTP bodies (`POST /api/sync`,
  `Content-Type: application/cbor`) are CBOR, encoded with `minicbor`.
- Structs are **integer-keyed maps**; unknown keys are ignored, so fields can be added without a
  protocol bump. Optional fields are omitted when absent.
- Enums are encoded as a 2-element array `[variant_index, payload]`. For struct-like variants the
  payload is an integer-keyed map; for tuple variants it is an array. `Reject` and `ErrorCode`
  are bare integers.
- `Id` = 16-byte byte string (UUIDv7). `Hash` = 32-byte byte string (SHA-256).
  `Hlc` = array `[wall_ms: u48, logical: u16, replica: u64]`, ordered lexicographically.
- Yjs updates are **v1** updates as byte strings. Text offsets are UTF-16 code units.

## Messages

| C→S | variant | fields (key: name) |
|---|---|---|
| `Hello` | 0 | 0 proto · 1 vault_id? · 2 replica_id · 3 token · 4 cursor · 5 app_version |
| `Push` | 1 | 0 ops: [Op] (≤ 1 MiB per frame; a group is never split) |
| `Pull` | 2 | 0 from · 1 limit_bytes |
| `DocSync` | 3 | 0 entry · 1 slot · 2 state_vector (Yjs v1) |
| `Ping` | 4 | 0 nonce |

| S→C | variant | fields |
|---|---|---|
| `Welcome` | 0 | 0 vault_id · 1 head_seq · 2 server_time · 3 min_client_proto · 4 hlc |
| `Ack` | 1 | 0 results: [[op_id, AckResult]] |
| `Changes` | 2 | 0 from · 1 to · 2 entries: [Entry] · 3 blobs: [BlobRow] · 4 docs: [DocUpdate] · 5 more · 6 hlc |
| `Pong` | 3 | 0 nonce · 1 server_time |
| `Error` | 4 | 0 code · 1 message · 2 retry_after_ms? |
| `DocDiff` | 5 | 0 entry · 1 slot · 2 update (server state minus the given state vector) |

`AckResult` = `Applied(seq)` 0 · `Duplicate(seq)` 1 · `Rejected(Reject)` 2.
`Reject` = Cycle 0 · ParentMissing 1 · ParentTrashed 2 · Purged 3 · NotTrashed 4 · BadName 5 ·
BadUpdate 6 · UnknownEntry 7 · GroupFailed 8 · Forbidden 9 · NotAFolder 10 · TooLarge 11.
`ErrorCode` = Unauthorized 0 · WrongVault 1 · ProtoTooOld 2 · BadFrame 3 · Busy 4 · Internal 5 · Revoked 6.

`DocUpdate` = map {0 seq · 1 entry · 2 slot · 3 bytes?}. **Missing `bytes` is the `Own` placeholder**:
the update came from the receiving replica and is not echoed back.

### Ops

`Op` = {0 op_id · 1 hlc · 2 known_seq · 3 group (0 = none) · 4 body}.
`OpBody` = `Meta(MetaOp)` 0 | `Doc {0 entry, 1 slot, 2 update}` 1.

| MetaOp | variant | fields |
|---|---|---|
| Create | 0 | 0 id · 1 kind · 2 parent? · 3 name · 4 tree_visible · 5 blob? · 6 blob_info? · 7 created_at? · 8 modified_at? · 9 props: [[key, cbor]] |
| SetParent | 1 | 0 id · 1 parent? |
| SetName | 2 | 0 id · 1 name |
| SetVisible | 3 | 0 id · 1 visible |
| SetBlob | 4 | 0 id · 1 blob · 2 blob_info? |
| Trash | 5 | 0 id |
| Restore | 6 | 0 target (trash batch id or entry id) |
| Purge | 7 | 0 id |
| SetProp | 8 | 0 id · 1 key · 2 value? (absent = delete) |
| SetTimes | 9 | 0 id · 1 created? · 2 modified? |

`BlobInfo` = {0 size · 1 mime? · 2 width? · 3 height? · 4 orientation?}.

### Rows

`Entry` = {0 id · 1 kind · 2 parent? · 3 name · 4 trashed? [batch, at] · 5 tree_visible · 6 blob? ·
7 created_at? · 8 modified_at? · 9 props {key: cbor} · 10 purged · 11 clock · 12 seq}.
`Clocks` = {0 parent · 1 name · 2 trashed · 3 visible · 4 blob · 5 created · 6 modified · 7 props {key: Hlc}}.
`BlobRow` = {0 hash · 1 size · 2 mime? · 3 width? · 4 height? · 5 orientation? · 6 present · 7 seq}.

## Session rules

1. The first frame must be `Hello` (within 5 s). The token is the device's bearer token; it is never
   put in a URL. A wrong vault, old protocol, bad or revoked token gets `Error` and the socket closes.
2. The server replies `Welcome`, then streams `Changes` from `Hello.cursor` up to the head, and keeps
   streaming after every commit.
3. **Cursor contract:** `Changes{from, to}` carries everything that changed in `(from, to]` (a row
   appears once, at its latest seq). A client applies a page only if `from == cursor`; otherwise it
   ignores it (if `to ≤ cursor`) or sends `Pull{from: cursor}`. Pages are cut at commit boundaries,
   so a rename and its link rewrites are never split. `more = true` means another page follows.
4. `Push` → `Ack` (after the batch is committed with `synchronous=FULL`) → `Changes`. Ops are
   deduplicated by `(replica_id, op_id)`; a replay gets its original outcome (`Duplicate(seq)`, or
   the original `Rejected`). Ops in the same non-zero `group` apply atomically.
   A doc op whose content is already covered by a purged note's final state is acked `Duplicate(0)`.
5. A client keeps a pending op until it is acknowledged **and** the cursor has reached the ack seq;
   rejected ops go to its quarantine (never discarded).
6. Liveness: client `Ping` every 10 s in the foreground, dead after 5 s without `Pong`; server
   WebSocket pings every 20 s and drops a silent connection after 45 s. Reconnect backoff: 0, 250 ms,
   500 ms, 1 s, 2 s, 4 s, 8 s, 15 s cap (60 s in background), ±20 % jitter.

## HTTP fallback

`POST /api/sync` with `HttpSyncRequest` {0 hello · 1 ops · 2 cursor · 3 wait_s (≤ 25)} returns
`HttpSyncResponse` {0 welcome · 1 acks · 2 changes: [Changes]}. With no ops and nothing new it
long-polls up to `wait_s`.

## Blob channel (JSON over HTTP, bearer token)

| request | response |
|---|---|
| `POST /api/blobs/{hash}/uploads` `{size}` | `200 {present:true}` or `201 {present:false, upload_id, chunk_size, received: hex bitmap}`; `413` over `JESS_MAX_UPLOAD_MB` |
| `PUT /api/blobs/{hash}/uploads/{id}/chunks/{i}` + `X-Chunk-SHA256`, body = bytes `[i·chunk, …)` | `200`; `404` unknown session; `422` chunk hash mismatch. Idempotent. |
| `POST /api/blobs/{hash}/uploads/{id}/complete` | `200 {present:true}`; `422` content mismatch (session discarded); `404` unknown |
| `GET`/`HEAD /api/blobs/{hash}` (+ `Range`) | bytes, `ETag: "<hash>"`, `Cache-Control: private, max-age=31536000, immutable` |
| `GET /api/blobs/{hash}/derived/{display\|thumb\|pdf-thumb\|pdf-text}` | derived data (phase 4) |
| `POST /api/blobs/presence` `{hashes:[hex]}` | `{present:[hex]}` |

Chunk size is 4 MiB. Bitmap bit *i* (LSB-first within each byte) = chunk *i* received.
Blob rows (`present`) travel in `Changes`, so clients learn when the server has a blob; a blob is
never evicted locally before that.

## Auth endpoints (JSON)

`GET /api/auth/state` · `POST /api/auth/setup {password, setup_code?, device_name?}` ·
`POST /api/auth/login {password, device_name?}` → `{token, device_id, vault_id}` ·
`POST /api/auth/pair` (bearer) → `{code, expires_at}` · `POST /api/auth/redeem {code, device_name?}` ·
`POST /api/auth/logout` · `GET /api/devices` · `POST /api/devices/{id}/revoke`.
Admin: `GET /api/admin/status`, `GET /api/admin/snapshot/latest`. Health: `GET /healthz`.

## Admin vault I/O (bearer token)

- `GET /api/admin/export.zip[?profile=portable][&trash=true]`: the whole vault as a streamed zip
  from one consistent read snapshot (Exact names by default; `portable` sanitises for Windows and
  adds `EXPORT-REPORT.md`).
- `POST /api/admin/import {zip_hash, conflict?, hide_pdfs_in_attachment_folder?, dry_run?}`:
  imports a zip previously uploaded through the blob channel (`zip_hash` = its SHA-256).
  `conflict` is `skip` (default), `keep_both` or `overwrite`; `dry_run: true` returns the plan and
  report without changing anything. Uses the same core planner as the clients (DESIGN §12.3).

## Golden encodings (hex)

- `hello`: `8200a100a50001020703617404182a056131`
- `push`: `8201a10081a5000101831903e8000702182a0300048200a1008202a20050010101010101010101010101010101010164612e6d64`
- `ack`: `8201a100828201820081182b820282028100`
