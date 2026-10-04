# Installing Jess Notes on Coolify

A step-by-step guide for Coolify v4 with its bundled Traefik proxy. The background (sizing,
every environment variable, operations, restoring snapshots) is in [DEPLOY.md](DEPLOY.md).

What you end up with: one container serving the sync server and the web app at
`https://notes.example.com`, one persistent volume (`/data`) holding the whole vault, and
HTTPS from Let's Encrypt.

Menu names below are Coolify v4's; they move a little between releases.

## Before you start

- **A Coolify server** with Docker. Running the app needs very little (512 MB of RAM is plenty
  for 10k notes). **Building the image is what's heavy**: it compiles the Rust server and the
  WebAssembly core with full optimisation. Plan on 4 GB of RAM (or 2 GB plus swap) and 15–30
  minutes on a 2-vCPU VPS for the first build. Later builds are faster, because Docker caches
  the layers. If your server is small, use [option B](#option-b-build-elsewhere-deploy-the-image)
  and build the image elsewhere.
- **A domain or subdomain**, e.g. `notes.example.com`, with a DNS `A` (and/or `AAAA`) record
  pointing at the Coolify server.
- **Access to this repository** from Coolify. It's private, so either connect a GitHub App in
  Coolify (*Sources → + Add → GitHub App*) or use a deploy key (step 2 below).

## Option A: build from the repository (recommended)

### 1. Create the resource

1. *Projects* → choose a project (or create one, e.g. "Notes") → an environment (*production*).
2. *+ New* → **Private Repository (with GitHub App)** if you connected one, otherwise
   **Private Repository (with Deploy Key)**.
3. Repository: `dochmccrum/jess-notes`, branch `main`.
4. **Build Pack: Dockerfile.** Not Nixpacks, and not Docker Compose (the compose file maps host
   port 8080, which Coolify doesn't need and which can clash with other apps). Dockerfile
   location: `/Dockerfile`, base directory `/`.

### 2. If you used a deploy key

Coolify shows a public key (or lets you pick one under *Keys & Tokens*). On GitHub: the
repository → *Settings → Deploy keys → Add deploy key*, paste it, **read-only** is enough.

### 3. Network

On the resource's *Configuration → General* page:

- **Domains:** `https://notes.example.com`. The `https://` makes Coolify request a Let's Encrypt
  certificate and redirect HTTP to HTTPS.
- **Ports Exposes:** `8080`.
- **Ports Mappings:** leave empty. Traefik reaches the container on the internal network;
  publishing 8080 on the host isn't needed.

### 4. Persistent storage (don't skip this)

*Configuration → Persistent Storage → + Add → Volume*:

- Name: e.g. `jess-data`
- **Destination path: `/data`**

Without it, every redeploy starts with an empty vault. Everything lives in `/data`: the
database, attachments, snapshots, the plain-files mirror and thumbnails.

### 5. Environment variables

*Configuration → Environment Variables*. All are optional; the useful ones:

| Variable | Suggested | Why |
|---|---|---|
| `JESS_ADMIN_PASSWORD` | a long password | Creates the account on first start, so you skip the one-time setup code. It's only read while no account exists. Delete it after the first deploy if you prefer. |
| `JESS_TRUST_PROXY` | `true` (default) | Rate limiting uses the client IP from Traefik's `X-Forwarded-For`. |
| `JESS_GIT_REMOTE` | `git@github.com:you/notes-backup.git` | Optional off-site history of your notes as plain markdown (see step 9). |
| `RUST_LOG` | `info` (default) | `debug` when troubleshooting. |

Mark `JESS_ADMIN_PASSWORD` as a secret (lock icon) so it doesn't show in logs. Untick *Build
Variable* for all of these: they're only needed when the container runs, not while it builds.

The rest (snapshot schedule, trash retention, upload limit, turning the mirror or thumbnails off)
are in [DEPLOY.md §2](DEPLOY.md#2-coolify).

### 6. Health check and shutdown

- **Health check:** the image has one built in (`jess health`, which calls `GET /healthz` on
  port 8080), and Docker uses it as it is. If you turn on Coolify's own health check
  (*Configuration → Healthcheck*), use path `/healthz`, port `8080`, scheme `http`.
- **Stop grace period:** on shutdown the server finishes in-flight writes, flushes the mirror and
  checkpoints the database, which takes up to ~15 s. If your Coolify version exposes a stop
  timeout or grace period under *Advanced*, set it to **30 s**. Docker's default of 10 s is
  usually enough for a quiet vault: nothing acknowledged to a device is ever lost, because
  every sync write is committed before the device hears back.

### 7. Deploy

Press **Deploy** and watch the build log. The first build downloads the Rust, Node and pdfium
toolchains and compiles everything; on a small server this is the slow part (see *Before you
start*).

When it's up, open the resource's **Logs**:

- If you set `JESS_ADMIN_PASSWORD`, you'll see `jess listening on 0.0.0.0:8080`. You're done.
- If not, look for

  ```
  SETUP CODE: XXXX-XXXX  (enter it on the setup screen to create the account)
  ```

  Open `https://notes.example.com`, enter the code and choose the vault password. The code
  works once.

Check that the volume works: create a note, press *Redeploy*, and make sure the note is still
there afterwards.

### 8. Automatic updates (optional)

With the GitHub App source, *Configuration → General → Auto Deploy* redeploys on every push to
`main`. With a deploy key, add the webhook Coolify shows under *Webhooks* to the GitHub
repository, or redeploy by hand.

Updates are safe to roll out at any time: the database migrates forward on start. Browsers pick
up the new web app on their next reload, and devices keep their local copies and catch up.

### 9. Backups

The vault is the `/data` volume. Coolify's scheduled backups cover database resources, not app
volumes, so set one up yourself:

- **Built in:** the server keeps compressed database snapshots in `/data/snapshots` (every 6 h,
  14 days). They protect against mistakes, not against losing the server.
- **Off-site plain files:** set `JESS_GIT_REMOTE` (step 5) to a private repository. After the
  first start, get the generated key

  ```sh
  docker exec <container> cat /data/git/deploy_key.pub
  ```

  and add it to that repository as a deploy key **with write access**. Notes are committed at
  most once a minute and pushed. Attachments are included only with
  `JESS_GIT_INCLUDE_ATTACHMENTS=true`.
- **The whole volume:** back up the Docker volume from the host. Find its path with
  `docker volume inspect <volume>` (Coolify names it after the resource, e.g.
  `<uuid>-jess-data`), and copy `jess.db*`, `blobs/` and `snapshots/` with restic, borg or your
  provider's snapshots. To restore a snapshot, see [DEPLOY.md §6](DEPLOY.md#6-operations).

The container name for `docker exec` is shown on the resource page. Coolify's *Terminal* tab
opens a shell in it too.

## Option B: build elsewhere, deploy the image

If the Coolify server is too small to build, build the image on a bigger machine and push it to
a registry. For example, GitHub's container registry (private by default):

```sh
docker build -t ghcr.io/dochmccrum/jess-notes:latest .
echo "$GITHUB_TOKEN" | docker login ghcr.io -u dochmccrum --password-stdin   # token with write:packages
docker push ghcr.io/dochmccrum/jess-notes:latest
```

In Coolify: *+ New* → **Docker Image** → `ghcr.io/dochmccrum/jess-notes:latest`. For a private
package, add the registry's credentials on the server first (`docker login ghcr.io` on the
Coolify host, with a token that has `read:packages`). Then do steps 3–7 above: domain and port
8080, the `/data` volume, environment variables, deploy. To update, push a new image and press
*Redeploy*.

## Connect your devices

- **Browser:** open `https://notes.example.com` and sign in with the vault password.
- **Linux app** (`.deb` or `.AppImage`) and **Android app:** on the welcome screen choose
  *On a Jess server*, enter `https://notes.example.com` and the password. To add a device
  without typing the password, open *Settings → Pair a device…* on a signed-in device: it shows
  a link and a QR code, valid once for 10 minutes. On Android, *Scan* reads the QR code.
- **Existing Obsidian vault:** on any signed-in device, *Settings → Import / export* (folder or
  .zip). It shows a dry-run report first, and nothing is overwritten without asking. For a very
  large vault already on the server, `jess import` is much faster, but it needs the server
  stopped: see [DEPLOY.md §4](DEPLOY.md#4-migrating-an-existing-obsidian-vault).

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| *Bad Gateway* / *No available server* | *Ports Exposes* isn't `8080`, or the container is still starting or crash-looping (check *Logs*). |
| Certificate errors | DNS doesn't point at the server yet, or the domain was entered without `https://`. |
| The vault is empty after a redeploy | No persistent volume at `/data` (step 4). The old data is in the previous container's anonymous volume, if it still exists. |
| The build is killed / "signal 9" / runs for an hour | Not enough RAM to compile: add swap, or use option B. |
| No setup code in the logs | An account already exists (the volume has data), or `JESS_ADMIN_PASSWORD` was set. Forgot the password: `docker exec -it <container> jess reset-password`. |
| Status bar says *Syncing* for a long time behind another proxy (e.g. Cloudflare) | WebSockets are blocked, so clients long-poll instead (it works, but slower). Allow WebSocket upgrades on `/api/sync`, and see DEPLOY.md *Proxy limits*. |
| Large attachments fail to upload behind another proxy | Allow request bodies of at least 5 MB (attachments upload in 4 MiB chunks). |
| Images show at full size, PDFs aren't searchable | Thumbnails and PDF text are made in the background after an import. Progress: `GET /api/admin/status` → `derive`. |

`docker exec <container> jess integrity-check` verifies the database, tree, documents, links
and attachments at any time.
