# Jess Notes

A fast, local-first markdown notes app that reads and writes an Obsidian vault, with a small
self-hosted sync server. The server also serves the web app. Your notes stay on your devices
and your server.

- **Fast:** the last note is on screen in ~0.2 s, even with 10,000 notes and 20,000
  attachments, and typing stays within a frame (built for 120 Hz screens).
- **Local-first:** every device has the whole vault (or just the notes, with attachments on
  demand) and works offline. Edits merge without conflicts (Yjs CRDTs), and an edit never loses
  to a delete on another device.
- **Obsidian-compatible:** `[[wikilinks]]`, embeds, tags, frontmatter, callouts and maths.
  Import a vault folder or zip and export it back byte for byte. Renaming a note rewrites the
  links to it.
- **Attachments and PDFs:** images inline at their size, a built-in PDF viewer that opens big
  scans fast, and full-text search across notes and PDF text.
- **Clients:** web (any modern browser), Linux (`.deb`, AppImage) and Android. On the apps you
  can also keep a vault on the device only, without a server.
- **Self-hosting:** one container, one port, one volume, in Rust and SQLite. It also keeps a
  plain-files copy of your vault and, optionally, git history pushed to a repository of yours.

One server holds one person's vault (with any number of devices); it isn't a multi-user service.

## Install the server

**Coolify:** *New Resource → Docker Compose Empty*, paste
[`docker-compose.coolify.yml`](docker-compose.coolify.yml), and *Deploy*. Coolify sets up the
domain, HTTPS, the volume and a generated password. Step by step: [docs/COOLIFY.md](docs/COOLIFY.md).

**Docker Compose**, behind any HTTPS reverse proxy:

```sh
curl -O https://raw.githubusercontent.com/dochmccrum/jess-notes/main/docker-compose.yml
docker compose up -d
docker compose logs jess | grep "SETUP CODE"   # enter it at http://<host>:8080 to create the vault
```

**Docker:**

```sh
docker run -d --name jess -p 8080:8080 -v jess-data:/data ghcr.io/dochmccrum/jess-notes:1
```

The image is `ghcr.io/dochmccrum/jess-notes`, for amd64 and arm64. Configuration, backups,
updates and proxies: [docs/DEPLOY.md](docs/DEPLOY.md).

## Apps

Download from the [releases](https://github.com/dochmccrum/jess-notes/releases):

- **Linux:** `jess-notes_*_amd64.deb` (Debian, Ubuntu) or the AppImage (anywhere). They include
  their own Chromium engine for smooth 120 Hz rendering, which makes the download large (~170 MB).
- **Android** (7.0+; Android System WebView 100+): `jess-notes-*-arm64.apk` (almost all phones)
  or `-armv7.apk` (older ones).
- **Web, iPad, other systems:** open your server's address in a browser.

In an app, choose *On a Jess server* and enter the address and password. A signed-in device can
add another one with *Settings → Pair a device…* (a link or QR code).

## Build from source

The Rust workspace (`core`, `server`, `apps/native`, `apps/tauri`) and the web UI (`ui/`, Svelte
and CodeMirror). Build and test commands are in [AGENTS.md](AGENTS.md), Android in
[docs/ANDROID.md](docs/ANDROID.md). The server image builds with `docker build .`.

## Documentation

- [docs/DEPLOY.md](docs/DEPLOY.md): running the server (configuration, backups, operations)
- [docs/COOLIFY.md](docs/COOLIFY.md): installing on Coolify, step by step
- [docs/DESIGN.md](docs/DESIGN.md): architecture and the decisions behind it
- [docs/PROTOCOL.md](docs/PROTOCOL.md): the sync protocol and HTTP API
- [CHANGELOG.md](CHANGELOG.md): releases
- [SECURITY.md](SECURITY.md): reporting a vulnerability

## License

[MIT](LICENSE)
