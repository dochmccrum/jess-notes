//! jess-server library: the engine (single writer), blob store, HTTP/WS API, background tasks.

pub mod auth;
pub mod blobfs;
pub mod changes;
pub mod config;
pub mod db;
pub mod engine;
pub mod error;
pub mod http;
pub mod integrity;
pub mod snapshot;
pub mod tasks;
pub mod writer;

/// Mirror hooks (implemented in phase 2).
pub fn start_mirror(_app: http::Shared) {}
pub async fn flush_mirror(_app: http::Shared) {}
pub fn rebuild_mirror(_cfg: &config::Config) -> error::Result<()> {
    Err(error::Error::Other("the mirror arrives in phase 2".into()))
}
pub fn mirror_check(_cfg: &config::Config) -> error::Result<Vec<String>> {
    Ok(vec![])
}
