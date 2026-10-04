# Deploying Jess Notes

One container, one port (8080), one volume (`/data`). The server also serves the web app, so
there is nothing else to deploy. These steps assume **Coolify** with its bundled Traefik
(which terminates TLS), but any Docker host behind an HTTPS reverse proxy works the same way.
**Step-by-step Coolify instructions are in [COOLIFY.md](COOLIFY.md)**; this file is the reference.

## 1. Sizing

| Vault | RAM | Disk |
|---|---|---|
| ≤ 10k notes, a few GB of attachments | 512 MB is plenty (idle ≈ 60–120 MB) | attachments × ~2 (blobs + plain-files mirror, hard-linked where possible) + snapshots |
| 10k–50k notes | 1 GB | same rule |

CPU: one shared vCPU is enough. Login hashing (argon2id, 64 MiB) briefly uses one core.

Everything lives in `/data`:

```
/data/jess.db            SQLite (WAL) – the source of truth
/data/blobs/             content-addressed attachments
/data/snapshots/         zstd-compressed database snapshots (every 6 h, 14 days)
/data/mirror/            the vault as plain files (+ .git if git is enabled)
/data/mirror-state.db    the mirror's own bookkeeping
/data/git/               the generated SSH deploy key for the git remote
/data/derived/           image display variants, thumbnails, PDF thumbnails and text (rebuildable)
```

The server derives image variants and PDF thumbnails/text once per file, in a background
subprocess with CPU/memory limits; progress is in `GET /api/admin/status` → `derive`. PDF work
needs the pdfium library, which the image includes (`/usr/local/lib/libpdfium.so`; override with
`JESS_PDFIUM_LIB`). Without it, PDFs still open and render in the apps; they just aren't
searchable and embeds have no ready-made thumbnail.

Back up the volume (or at least `jess.db`, `blobs/` and `snapshots/`) with whatever your host
provides. The mirror can always be rebuilt (`jess rebuild-mirror`).

## 2. Coolify

1. **New resource → Public/Private repository** (this repo) → build pack **Dockerfile**.
   (Or **Docker Compose** with `docker-compose.yml`; both work.)
2. **Ports**: expose `8080`. Attach your domain, e.g. `https://notes.example.com`; Coolify
   configures Traefik and Let's Encrypt.
3. **Storage**: add a persistent volume mounted at `/data`. *Without it every redeploy starts an
   empty vault.*
4. **Environment variables** (all optional):

   | Variable | Default | Meaning |
   |---|---|---|
   | `JESS_ADMIN_PASSWORD` | – | Create the account with this password on first start (skips the setup code). Remove it afterwards if you like; it is only used when no account exists. |
   | `JESS_GIT_REMOTE` | – | e.g. `git@github.com:you/notes-backup.git`. The mirror is committed every minute and pushed here. |
   | `JESS_GIT_INCLUDE_ATTACHMENTS` | `false` | Commit attachments too (large!). `JESS_GIT_LFS=true` stores them with Git LFS. |
   | `JESS_MIRROR_ENABLED` / `JESS_GIT_ENABLED` | `true` | Turn the plain-files mirror / local git history off. |
   | `JESS_DERIVE` | `true` | `false` stops the background thumbnails and PDF text (images then show at full size, PDFs aren't searchable). |
   | `JESS_MAX_UPLOAD_MB` | `2048` | Largest single attachment. |
   | `JESS_SNAPSHOT_INTERVAL_HOURS` / `JESS_SNAPSHOT_RETENTION_DAYS` | `6` / `14` | Database snapshots. |
   | `JESS_TRASH_RETENTION_DAYS` | `30` | Trash is emptied after this. |
   | `JESS_BLOB_RETENTION_DAYS` | `30` | Unreferenced attachments are kept at least this long (always ≥ snapshot retention + 7). |
   | `JESS_TRUST_PROXY` | `true` | Use `X-Forwarded-For` for client IPs (login rate limiting). Set `false` if the port is exposed without a proxy. |
   | `RUST_LOG` | `info` | Log level. |

5. **Stop timeout**: if your Coolify version has a stop timeout / grace period setting, use
   **30 s**. On SIGTERM the server finishes in-flight writes, flushes the mirror (≤ 10 s) and
   checkpoints the database (acknowledged writes are already committed, so a shorter timeout
   never loses synced data).
6. **Health check**: the image has one built in (`jess health` → `GET /healthz`). Coolify picks
   it up automatically; nothing to configure.
7. Deploy. Then open the **logs** once and look for:

   ```
   SETUP CODE: XXXX-XXXX  (enter it on the setup screen to create the account)
   ```

   Open your domain, enter the setup code and choose the vault password. (Skip this if you
   set `JESS_ADMIN_PASSWORD`.)

### Proxy limits (Traefik)

Coolify's Traefik defaults work: WebSockets are passed through, and attachments upload in
4 MiB chunks, so no body-size limit needs raising. If you put another proxy in front
(Cloudflare, nginx):

- allow WebSocket upgrades on `/api/sync` (if they're blocked, clients fall back to HTTP
  long-polling automatically, which is slower but works);
- allow request bodies of at least **5 MB** (one chunk plus headers);
- set idle/read timeouts to at least **60 s** (the long-poll waits up to 25 s; clients ping
  over the WebSocket every 10 s, which also keeps idle connections open);
- don't buffer responses for `/api/blobs/*` and `/api/admin/export.zip` (they're streamed).

## 3. Signing in on devices

- **Web**: open the domain and sign in with the vault password. Each browser is a separate
  device that you can revoke in Settings → Devices.
- **Another device without typing the password**: Settings → *Pair a device…* shows a link that
  is valid for 10 minutes and works once.
- Lost a device? Revoke it in Settings → Devices; it is disconnected immediately.
- Forgot the password? On the server: `docker exec -it <container> jess reset-password`
  (reads the new password from stdin or `JESS_NEW_PASSWORD`; existing devices stay signed in).

## 4. Migrating an existing Obsidian vault

Use the app: Settings → *Import / export…* → *Choose folder…* (Chromium browsers) or *Choose
.zip…* (everywhere; the single top-level folder inside a zip is stripped automatically). It shows
a dry-run report first and never overwrites anything without asking. Large vaults are streamed
and imported in batches, so a multi-GB vault is fine.

For automation there is also an admin API (`POST /api/admin/import` with a zip already uploaded
as a blob), described in `docs/PROTOCOL.md`.

A vault already on the server's disk can be imported there, with the same planner, while the
server is stopped (about 15 s for 10k notes and 20k images):

```sh
jess import /path/to/vault --dry-run    # the report only
jess import /path/to/vault              # or a .zip; --conflict skip|overwrite|keep_both
jess derive-all                         # optional: thumbnails and PDF text now, not in the background
```

What is imported: every note byte-for-byte (including line endings and BOMs), folders (empty
ones too), PDFs, images and other files, plus the vault's attachment settings from
`.obsidian/app.json`. What is skipped (and listed in the report): `.obsidian/`, `.git/`,
`.trash/`, `.DS_Store` and similar. Export (Settings → *Export as .zip*) gives the same files
back unchanged.

## 5. Git backup of the plain files

With `JESS_GIT_REMOTE` set, the server generates an SSH deploy key on first start. Get the public
key from the admin status (`GET /api/admin/status` → `git.deploy_key`) or:

```sh
docker exec <container> cat /data/git/deploy_key.pub
```

Add it as a **deploy key with write access** to a private repository. The mirror is committed at
most once a minute (`JESS_GIT_COMMIT_INTERVAL`) and pushed; failures are shown in the admin
status and retried, and never affect syncing.

## 6. Operations

```sh
docker exec <container> jess integrity-check            # database, tree, docs, links, blobs
docker exec <container> jess integrity-check --hashes   # also re-hash every attachment (slow)
docker exec <container> jess integrity-check --mirror   # also compare the mirror with the database
docker exec <container> jess snapshot                   # take a snapshot now
docker exec <container> jess rebuild-mirror             # rebuild /data/mirror from scratch
docker exec <container> jess gc --dry-run               # what attachment GC would delete
```

**Restoring a snapshot**: stop the container, then in the volume:

```sh
zstd -d /data/snapshots/jess-<time>.db.zst -o /data/jess.db.restore
mv /data/jess.db /data/jess.db.broken && rm -f /data/jess.db-wal /data/jess.db-shm
mv /data/jess.db.restore /data/jess.db
```

(`zstd` isn't in the image: run this on the host, or in any container that mounts the volume.)
Start the container and run `jess integrity-check`.

Devices that synced changes *newer* than the snapshot notice that the server went back in time.
They stop syncing and say so in the status bar ("server is behind this device") rather than
guess, so nothing is silently lost. To bring such a device back:

1. On that device, *Settings → Import / export → Export as .zip* (this saves everything it has,
   including the newer changes).
2. *Settings → Erase this device's local copy…* and sign in again. The device downloads the
   restored vault.
3. *Import* the zip from step 1 with conflicts set to **Keep both** (or **Skip** for notes you
   know are unchanged). Nothing identical is duplicated.

## 7. Updating

Redeploy in Coolify. The database schema migrates forward automatically on start; the web app
updates itself on the next load (the service worker fetches the new version and uses it after a
reload).
