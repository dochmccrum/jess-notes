# Installing Jess Notes on Coolify

Jess Notes runs as one container: the sync server and the web app, with one volume holding the
whole vault. Coolify gives it a domain, HTTPS (Let's Encrypt through its Traefik proxy) and that
volume, all from one compose file. Reference for everything else (configuration, backups,
operations): [DEPLOY.md](DEPLOY.md).

Menu names below are Coolify v4's; they move a little between releases.

## You need

- A Coolify server. 512 MB of free RAM is plenty for 10k notes, on amd64 or arm64.
- Optionally your own domain: an `A`/`AAAA` record for e.g. `notes.example.com` pointing at the
  server. Without one, Coolify gives you a generated `sslip.io` address to start with.

## Install (about two minutes)

1. *Projects* → your project → environment → **+ New** → **Docker Compose Empty**.
2. Paste the contents of [`docker-compose.coolify.yml`](../docker-compose.coolify.yml) and save.
3. **Domain:** on the `jess` service's settings, set it to `https://notes.example.com`, or keep
   the generated one. The `https://` gets a certificate and redirects HTTP. The port is already
   set (8080) by `SERVICE_FQDN_JESS_8080` in the file.
4. **Deploy.** Coolify pulls the image (`ghcr.io/dochmccrum/jess-notes:1`), creates the
   `jess-data` volume and keeps it across redeploys and updates.
5. **Your password:** *Environment Variables* → `SERVICE_PASSWORD_JESS`. Coolify generated it,
   and it's the vault password. Put it in your password manager, or change it after signing in
   (*Settings → Password*).
6. Open your domain and sign in.

That's it: the volume, the port, HTTPS and the first account need no further steps. The image's
built-in health check (`/healthz`) tells Coolify when it's up.

Check that it persists: create a note, *Redeploy*, and make sure the note is still there.

### Updates

`:1` follows the newest 1.x release. To update, press **Redeploy** (Coolify pulls the image
again). The database migrates forward on start, browsers pick up the new web app on their next
reload, and the apps carry on syncing. To pin a version, change the image tag to e.g. `:1.0.0`.

## Options

Add these on the *Environment Variables* tab, or uncomment them in the compose file. All are
optional; the full list is in [DEPLOY.md §2](DEPLOY.md#2-coolify).

| Variable | What for |
|---|---|
| `JESS_GIT_REMOTE` | e.g. `git@github.com:you/notes-backup.git`: your notes as plain files, committed every minute and pushed to a private repository (see *Backups*). |
| `JESS_GIT_INCLUDE_ATTACHMENTS` | `true` to commit attachments too (large). |
| `JESS_MAX_UPLOAD_MB` | Largest attachment (default 2048). |
| `JESS_SNAPSHOT_INTERVAL_HOURS` / `JESS_SNAPSHOT_RETENTION_DAYS` | Database snapshots (default every 6 h, kept 14 days). |
| `RUST_LOG` | `debug` when troubleshooting. |

`JESS_ADMIN_PASSWORD` (the generated password) is only used to create the account on the very
first start. Changing it later does nothing: change the password in the app instead, or reset it
in the container's terminal with `jess reset-password`.

## Connect your devices

- **Browser:** open your domain and sign in.
- **Linux and Android apps** (from the
  [releases](https://github.com/dochmccrum/jess-notes/releases)): on the welcome screen choose
  *On a Jess server* and enter the address and password. To add a device without typing the
  password, open *Settings → Pair a device…* on a signed-in one: it shows a link and a QR code,
  valid once for 10 minutes. The Android app scans it.
- **Existing Obsidian vault:** on any signed-in device, *Settings → Import / export* → choose the
  folder or a .zip. It shows a dry-run report first, and nothing is overwritten without asking.

## Backups

Everything is in the `jess-data` volume. Coolify backs up its database resources, not app
volumes, so back this one up yourself:

- **Built in:** compressed database snapshots in `/data/snapshots` (every 6 h, kept 14 days).
  They protect against mistakes, not against losing the server.
- **Off-site, as plain files:** set `JESS_GIT_REMOTE` to a private repository and redeploy. Then,
  in the resource's *Terminal* (or `docker exec <container> …` on the host), run
  `cat /data/git/deploy_key.pub` and add that key to the repository as a deploy key **with write
  access**. Notes are committed at most once a minute and pushed.
- **The whole volume:** on the host, `docker volume ls | grep jess-data` finds it, and
  `docker volume inspect` gives its path. Back up `jess.db*`, `blobs/` and `snapshots/` with
  restic, borg or your provider's snapshots. To restore a snapshot, see
  [DEPLOY.md §6](DEPLOY.md#6-operations).

## Building it yourself instead

To run your own build (a fork, or unreleased changes): *+ New* → **Public Repository** →
`https://github.com/dochmccrum/jess-notes` (or your fork), build pack **Dockerfile**. Then, on
the resource:

- *Ports Exposes:* `8080`, and set the domain.
- *Persistent Storage → + Add → Volume*, destination **`/data`**. Without it, every redeploy
  starts an empty vault.
- *Environment Variables:* `JESS_ADMIN_PASSWORD` (mark it as a secret, untick *Build Variable*),
  or leave it out and take the one-time `SETUP CODE` from the logs on first start.

Building compiles the Rust server and WebAssembly core with full optimisation: plan on 4 GB of
RAM (or 2 GB plus swap) and 15–30 minutes on a 2-vCPU server for the first build.

Alternatively, use **Docker Compose** with the repository and `docker-compose.coolify.yml`,
after changing its `image:` line to `build: .`.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| *Bad Gateway* / *No available server* | The container is still starting (the health check needs a few seconds) or is crash-looping: check *Logs*. For the Dockerfile route: *Ports Exposes* must be `8080`. |
| Certificate errors | DNS doesn't point at the server yet, or the domain was entered without `https://`. |
| "Wrong password" with the generated one | The account already existed when the password was generated, e.g. the volume came from an earlier install. Sign in with the old password, or run `jess reset-password` in the *Terminal*. |
| The vault is empty after a redeploy | Dockerfile route without a `/data` volume (the compose route always has one). |
| A Dockerfile build is killed ("signal 9") or runs for an hour | Not enough RAM to compile: add swap, or use the compose route (no build). |
| The status bar says *Syncing* for a long time behind another proxy (e.g. Cloudflare) | WebSockets are blocked, so clients long-poll instead (slower, but it works). Allow WebSocket upgrades on `/api/sync`; see DEPLOY.md *Proxy limits*. |
| Images show at full size, PDFs aren't searchable | Thumbnails and PDF text are made in the background after an import. Progress: `GET /api/admin/status` → `derive`. |

In the *Terminal*, `jess integrity-check` verifies the database, tree, documents, links and
attachments at any time.
