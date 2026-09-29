//! Process harness for end-to-end and crash tests: runs the real `jess` binary.
#![allow(dead_code)]

use futures_util::{SinkExt, StreamExt};
use jess_core::client::{Client, Output};
use jess_core::kv::Write;
use jess_core::proto::{self, ServerMsg};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const PASSWORD: &str = "correct horse battery";

pub struct Server {
    pub child: Child,
    pub port: u16,
    pub dir: PathBuf,
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl Server {
    pub fn start(dir: &Path) -> Server {
        Server::start_on(dir, free_port())
    }
    pub fn start_on(dir: &Path, port: u16) -> Server {
        let child = Command::new(env!("CARGO_BIN_EXE_jess"))
            .arg("serve")
            .env("JESS_DATA_DIR", dir)
            .env("PORT", port.to_string())
            .env("JESS_ADMIN_PASSWORD", PASSWORD)
            .env("JESS_UI_DIR", dir.join("no-ui"))
            .env("RUST_LOG", "warn")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn jess");
        let s = Server {
            child,
            port,
            dir: dir.to_path_buf(),
        };
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(20) {
            if let Ok(r) = ureq::get(&s.url("/healthz")).call() {
                if r.status() == 200 {
                    return s;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("server did not become healthy");
    }
    pub fn url(&self, p: &str) -> String {
        format!("http://127.0.0.1:{}{p}", self.port)
    }
    pub fn ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}/api/sync", self.port)
    }
    pub fn kill9(&mut self) {
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
    pub fn stop(&mut self) {
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        let _ = self.child.wait();
    }
    pub fn login(&self, name: &str) -> String {
        let r: serde_json::Value = ureq::post(&self.url("/api/auth/login"))
            .send_json(serde_json::json!({ "password": PASSWORD, "device_name": name }))
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap();
        r["token"].as_str().unwrap().to_string()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn integrity(dir: &Path, hashes: bool) -> (bool, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_jess"));
    c.arg("integrity-check")
        .env("JESS_DATA_DIR", dir)
        .env("RUST_LOG", "error");
    if hashes {
        c.arg("--hashes");
    }
    let o = c.output().unwrap();
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr),
    )
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// A real `core` client talking to the server over WebSocket.
pub struct WsClient {
    pub c: Client,
    pub kv: BTreeMap<Vec<u8>, Vec<u8>>,
    pub token: String,
    ws: Option<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    pub remote_updates: Vec<(jess_core::Id, Vec<u8>, Instant)>,
}

impl WsClient {
    pub fn new(token: String, replica: u64) -> WsClient {
        let (c, w) = Client::new(replica, jess_core::blobs::CHUNK_SIZE);
        let mut me = WsClient {
            c,
            kv: BTreeMap::new(),
            token,
            ws: None,
            remote_updates: vec![],
        };
        me.commit(w);
        me
    }
    pub fn commit(&mut self, w: Vec<Write>) {
        for x in w {
            match x {
                Write::Put(k, v) => {
                    self.kv.insert(k, v);
                }
                Write::Del(k) => {
                    self.kv.remove(&k);
                }
            }
        }
    }
    pub async fn handle(&mut self, o: Output) {
        self.commit(o.writes);
        for e in o.events {
            if let jess_core::client::Event::DocRemote { entry, update, .. } = e {
                self.remote_updates.push((entry, update, Instant::now()));
            }
        }
        if let Some(ws) = self.ws.as_mut() {
            for m in o.send {
                let _ = ws
                    .send(tokio_tungstenite::tungstenite::Message::Binary(
                        proto::encode(&m).into(),
                    ))
                    .await;
            }
        }
    }
    pub async fn connect(&mut self, url: &str) {
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("ws connect");
        self.ws = Some(ws);
        let o = self.c.connected(&self.token, "test");
        self.handle(o).await;
    }
    /// Processes incoming frames for up to `d`.
    pub async fn pump(&mut self, d: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + d;
        loop {
            let Some(ws) = self.ws.as_mut() else {
                return false;
            };
            let r = tokio::time::timeout_at(deadline, ws.next()).await;
            match r {
                Err(_) => return true,
                Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(b)))) => {
                    let m: ServerMsg = proto::decode(&b).expect("decode");
                    let o = self.c.on_message(m, now_ms());
                    self.handle(o).await;
                }
                Ok(Some(Ok(_))) => {}
                _ => {
                    self.ws = None;
                    let o = self.c.disconnected();
                    self.commit(o.writes);
                    return false;
                }
            }
        }
    }
    /// Pumps until `pred` holds or the timeout expires.
    pub async fn until(
        &mut self,
        timeout: Duration,
        mut pred: impl FnMut(&Client) -> bool,
    ) -> bool {
        let t = Instant::now();
        while t.elapsed() < timeout {
            if pred(&self.c) {
                return true;
            }
            if !self.pump(Duration::from_millis(20)).await && self.ws.is_none() {
                return pred(&self.c);
            }
        }
        pred(&self.c)
    }
    pub fn doc_text(&self, id: jess_core::Id) -> String {
        let prefix = jess_core::kv::doc_prefix(id, "body");
        let d = jess_core::doc::new_doc(77);
        for (_, v) in self
            .kv
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
        {
            jess_core::doc::apply(&d, v).unwrap();
        }
        for u in self.c.pending_doc_updates(id, "body") {
            jess_core::doc::apply(&d, &u).unwrap();
        }
        jess_core::doc::text(&d)
    }
}
