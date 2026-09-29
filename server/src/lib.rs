//! jess-server library: the engine (single writer), blob store, HTTP/WS API, background tasks.

pub mod auth;
pub mod blobfs;
pub mod changes;
pub mod config;
pub mod db;
pub mod derive;
pub mod engine;
pub mod error;
pub mod http;
pub mod integrity;
pub mod snapshot;
pub mod tasks;
pub mod writer;

pub mod mirror_task;

pub fn start_mirror(app: http::Shared) {
    mirror_task::start(app)
}
pub async fn flush_mirror(app: http::Shared) {
    mirror_task::flush(app).await
}
pub fn rebuild_mirror(cfg: &config::Config) -> error::Result<()> {
    mirror_task::rebuild(cfg)
}
pub fn mirror_check(cfg: &config::Config) -> error::Result<Vec<String>> {
    mirror_task::check(cfg)
}
pub mod git;
pub mod mirror;
pub mod vault_io;
