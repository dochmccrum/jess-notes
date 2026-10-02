//! Spaces (DESIGN §24): the vaults this device holds, one open at a time. A remote space syncs
//! with a Jess server; a local one runs that same server in-process on loopback, so both kinds
//! share one engine (op validation, link rewriting, delete/edit rules).
//!
//! On disk, under the app's data folder: `spaces.json` (the registry) and `spaces/<id>/` per
//! space, with the native client's store in `data/` and, for a local space, the embedded
//! server's in `server/`.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Local,
    Remote,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Space {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// Remote: the server's base URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Local: the embedded server's password (random; the client signs in with it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// Remote: a device token obtained while adding the space, handed to the client on first open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_token: Option<String>,
}

impl Space {
    /// What the UI may see (no secrets).
    pub fn public(&self) -> Value {
        json!({ "id": self.id, "name": self.name, "kind": self.kind, "server": self.server })
    }
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Registry {
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub spaces: Vec<Space>,
}

fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut b = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

impl Registry {
    fn path(root: &Path) -> PathBuf {
        root.join("spaces.json")
    }

    /// Loads the registry. A device from before spaces had one remote vault in `<root>/data`:
    /// it becomes the first space, keeping its data, server and token.
    pub fn load(root: &Path) -> Result<Registry, String> {
        let p = Self::path(root);
        if p.exists() {
            let s = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            return serde_json::from_str(&s).map_err(|e| format!("{}: {e}", p.display()));
        }
        let mut r = Registry::default();
        let old = root.join("data");
        if old.exists() {
            let s = r.add(root, "My notes", Kind::Remote)?;
            let id = s.id.clone();
            std::fs::rename(&old, data_dir(root, &id)).map_err(|e| format!("moving data: {e}"))?;
            r.active = Some(id);
            r.save(root)?;
        }
        Ok(r)
    }

    /// Written to a temporary file and renamed: a crash never leaves a half-written registry.
    pub fn save(&self, root: &Path) -> Result<(), String> {
        std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let p = Self::path(root);
        let tmp = p.with_extension("json.tmp");
        let s = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, s).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
    }

    /// Adds a space (not saved, not made active) and creates its directory.
    pub fn add(&mut self, root: &Path, name: &str, kind: Kind) -> Result<&mut Space, String> {
        let id = random_hex(8);
        std::fs::create_dir_all(data_dir(root, &id)).map_err(|e| e.to_string())?;
        let name = name.trim();
        self.spaces.push(Space {
            id,
            name: if name.is_empty() {
                "Notes".into()
            } else {
                name.into()
            },
            kind,
            server: None,
            secret: (kind == Kind::Local).then(|| random_hex(24)),
            pending_token: None,
        });
        Ok(self.spaces.last_mut().expect("just pushed"))
    }

    pub fn get(&self, id: &str) -> Option<&Space> {
        self.spaces.iter().find(|s| s.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Space> {
        self.spaces.iter_mut().find(|s| s.id == id)
    }

    pub fn active(&self) -> Option<&Space> {
        self.active.as_deref().and_then(|id| self.get(id))
    }

    /// Removes a space and all its data. The open one can't be removed (switch first).
    pub fn remove(&mut self, root: &Path, id: &str) -> Result<(), String> {
        if self.active.as_deref() == Some(id) {
            return Err("switch to another space before deleting this one".into());
        }
        let before = self.spaces.len();
        self.spaces.retain(|s| s.id != id);
        if self.spaces.len() == before {
            return Err("no such space".into());
        }
        let dir = space_dir(root, id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn public(&self) -> Value {
        json!({
            "active": self.active,
            "spaces": self.spaces.iter().map(Space::public).collect::<Vec<_>>(),
        })
    }
}

pub fn space_dir(root: &Path, id: &str) -> PathBuf {
    root.join("spaces").join(id)
}

/// The native client's store for a space.
pub fn data_dir(root: &Path, id: &str) -> PathBuf {
    space_dir(root, id).join("data")
}

/// The embedded server's data for a local space.
pub fn server_dir(root: &Path, id: &str) -> PathBuf {
    space_dir(root, id).join("server")
}

// ------------------------------------------------------------------ the embedded server

/// A local space's server, serving on `127.0.0.1:<port>` from its own thread and runtime until
/// dropped (which stops it and checkpoints its database).
pub struct Embedded {
    pub url: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Embedded {
    /// `derive`: run the attachment-derivation worker. It re-executes the current binary as
    /// `<exe> derive …`, so only a binary that answers that may enable it (the Linux app does).
    pub fn start(dir: &Path, secret: &str, derive: bool) -> Result<Embedded, String> {
        let mut cfg = jess_server::config::Config::from_env();
        cfg.data_dir = dir.to_path_buf();
        cfg.ui_dir = dir.join("no-ui");
        cfg.admin_password = Some(secret.to_string());
        // The space is already on this device: no mirror, no git. Snapshots stay (its history).
        cfg.mirror_enabled = false;
        cfg.git_enabled = false;
        cfg.git_remote = None;
        cfg.trust_proxy = false;
        cfg.login_per_minute = 1000;
        // A random 192-bit secret, not a password: no slow KDF (seconds in debug builds).
        cfg.random_secret = true;
        let (tx, rx) = std::sync::mpsc::channel::<Result<u16, String>>();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("jess-local-space".into())
            .spawn(move || {
                let app = match jess_server::serve::build(&cfg) {
                    Ok(a) => a,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                };
                rt.block_on(async move {
                    let l = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
                        Ok(l) => l,
                        Err(e) => {
                            let _ = tx.send(Err(e.to_string()));
                            return;
                        }
                    };
                    let port = l.local_addr().map(|a| a.port()).unwrap_or(0);
                    let _ = tx.send(Ok(port));
                    let shutdown = async move {
                        let _ = stopped.await;
                    };
                    if let Err(e) = jess_server::serve::run(app, l, shutdown, derive).await {
                        tracing::error!("local space server: {e}");
                    }
                });
            })
            .map_err(|e| e.to_string())?;
        let port = rx
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| "the local space's server didn't start".to_string())??;
        Ok(Embedded {
            url: format!("http://127.0.0.1:{port}"),
            stop: Some(stop),
            thread: Some(thread),
        })
    }
}

impl Drop for Embedded {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ------------------------------------------------------------------ signing in

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn post_json(url: &str, token: Option<&str>, body: Value) -> Result<Value, String> {
    let mut r = agent().post(url);
    if let Some(t) = token {
        r = r.header("authorization", &format!("Bearer {t}"));
    }
    let mut resp = r.send_json(body).map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        let msg = v["error"].as_str().map(str::to_string).unwrap_or(text);
        return Err(match status {
            401 | 403 => "wrong password or expired pairing code".into(),
            429 => "too many attempts: wait a minute".into(),
            _ => format!("the server said {status}: {msg}"),
        });
    }
    Ok(v)
}

/// The server's base URL and, if `input` is a pairing link (`https://host/#pair=CODE`), its code.
pub fn parse_server(input: &str) -> Result<(String, Option<String>), String> {
    let s = input.trim();
    let (base, pair) = match s.split_once("#pair=") {
        Some((b, c)) => (b, Some(c.split('&').next().unwrap_or("").to_string())),
        None => (s, None),
    };
    let base = base.trim_end_matches('/').to_string();
    if !(base.starts_with("https://") || base.starts_with("http://")) {
        return Err("the address must start with https:// (or http://)".into());
    }
    Ok((base, pair.filter(|c| !c.is_empty())))
}

/// Signs this device in: with a pairing code if there is one, else with the password.
/// Returns a device token.
pub fn sign_in(
    server: &str,
    password: Option<&str>,
    pair_code: Option<&str>,
    device_name: &str,
) -> Result<String, String> {
    // A server with no account yet needs its password chosen first (with the setup code from
    // its log), which the server's own page does.
    if pair_code.is_none() {
        if let Ok(mut r) = agent().get(&format!("{server}/api/auth/state")).call() {
            let st: Value = r.body_mut().read_json().unwrap_or(Value::Null);
            if st["needs_setup"] == true {
                return Err(format!(
                    "this server has no account yet: open {server} in a browser to choose its password, then add it here"
                ));
            }
        }
    }
    let v = match (pair_code, password) {
        (Some(code), _) => post_json(
            &format!("{server}/api/auth/redeem"),
            None,
            json!({ "code": code, "device_name": device_name }),
        )?,
        (None, Some(pw)) => post_json(
            &format!("{server}/api/auth/login"),
            None,
            json!({ "password": pw, "device_name": device_name }),
        )?,
        (None, None) => return Err("a password or a pairing link is needed".into()),
    };
    v["token"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "the server didn't return a device token".into())
}

// ------------------------------------------------------------------ moving to a server

/// Uploads `zip` as a blob to `server` and imports it there (keeping both copies on name
/// clashes). Returns the server's import report.
pub fn import_zip(server: &str, token: &str, zip: &Path) -> Result<Value, String> {
    let bytes = std::fs::read(zip).map_err(|e| e.to_string())?;
    let hash = hex::encode(Sha256::digest(&bytes));
    let begin = post_json(
        &format!("{server}/api/blobs/{hash}/uploads"),
        Some(token),
        json!({ "size": bytes.len() }),
    )?;
    if !begin["present"].as_bool().unwrap_or(false) {
        let id = begin["upload_id"]
            .as_str()
            .ok_or("upload refused")?
            .to_string();
        let cs = begin["chunk_size"].as_u64().unwrap_or(4 << 20) as usize;
        for (i, c) in bytes.chunks(cs.max(1)).enumerate() {
            let mut resp = agent()
                .put(&format!(
                    "{server}/api/blobs/{hash}/uploads/{id}/chunks/{i}"
                ))
                .header("authorization", &format!("Bearer {token}"))
                .header("x-chunk-sha256", &hex::encode(Sha256::digest(c)))
                .header("content-type", "application/octet-stream")
                .send(c)
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                let t = resp.body_mut().read_to_string().unwrap_or_default();
                return Err(format!("upload chunk {i}: {} {t}", resp.status()));
            }
        }
        post_json(
            &format!("{server}/api/blobs/{hash}/uploads/{id}/complete"),
            Some(token),
            json!({}),
        )?;
    }
    post_json(
        &format!("{server}/api/admin/import"),
        Some(token),
        json!({ "zip_hash": hash, "conflict": "keep_both" }),
    )
}

/// The attachment-derivation subprocess (`<exe> derive <job> <blob> <out>`): a binary that runs
/// a local space with derivation on must answer this, like `jess derive` (DESIGN §8, §24.1).
pub fn derive_cli(args: &[String]) -> Result<(), String> {
    jess_server::derive::cli(args)
}
