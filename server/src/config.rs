//! Environment configuration (DESIGN §15).

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    pub data_dir: PathBuf,
    pub ui_dir: PathBuf,
    pub admin_password: Option<String>,
    pub max_upload_mb: u64,
    pub mirror_enabled: bool,
    pub git_enabled: bool,
    pub git_remote: Option<String>,
    pub git_commit_interval_s: u64,
    pub git_include_attachments: bool,
    pub git_lfs: bool,
    pub snapshot_interval_hours: u64,
    pub snapshot_retention_days: u64,
    pub blob_retention_days: u64,
    pub trash_retention_days: u64,
    /// Trust the last `X-Forwarded-For` hop for client IPs (behind Traefik).
    pub trust_proxy: bool,
    /// Login/setup/redeem attempts per IP per minute (DESIGN §14: 5).
    pub login_per_minute: u64,
}

fn env_bool(k: &str, d: bool) -> bool {
    match std::env::var(k) {
        Ok(v) => matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        Err(_) => d,
    }
}
fn env_u64(k: &str, d: u64) -> u64 {
    std::env::var(k)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(d)
}

impl Config {
    pub fn from_env() -> Config {
        let data_dir =
            PathBuf::from(std::env::var("JESS_DATA_DIR").unwrap_or_else(|_| "/data".into()));
        let ui_dir = std::env::var("JESS_UI_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let local = PathBuf::from("ui/dist");
                if local.exists() {
                    local
                } else {
                    PathBuf::from("/app/ui")
                }
            });
        let snapshot_retention_days = env_u64("JESS_SNAPSHOT_RETENTION_DAYS", 14);
        // Blob retention always exceeds snapshot retention + 7 days, so restoring any snapshot
        // never refers to a deleted blob (§7.8).
        let blob_retention_days =
            env_u64("JESS_BLOB_RETENTION_DAYS", 30).max(snapshot_retention_days + 7);
        Config {
            port: env_u64("PORT", 8080) as u16,
            data_dir,
            ui_dir,
            admin_password: std::env::var("JESS_ADMIN_PASSWORD")
                .ok()
                .filter(|s| !s.is_empty()),
            max_upload_mb: env_u64("JESS_MAX_UPLOAD_MB", 2048),
            mirror_enabled: env_bool("JESS_MIRROR_ENABLED", true),
            git_enabled: env_bool("JESS_GIT_ENABLED", true),
            git_remote: std::env::var("JESS_GIT_REMOTE")
                .ok()
                .filter(|s| !s.is_empty()),
            git_commit_interval_s: env_u64("JESS_GIT_COMMIT_INTERVAL", 60),
            git_include_attachments: env_bool("JESS_GIT_INCLUDE_ATTACHMENTS", false),
            git_lfs: env_bool("JESS_GIT_LFS", false),
            snapshot_interval_hours: env_u64("JESS_SNAPSHOT_INTERVAL_HOURS", 6),
            snapshot_retention_days,
            blob_retention_days,
            trash_retention_days: env_u64("JESS_TRASH_RETENTION_DAYS", 30),
            trust_proxy: env_bool("JESS_TRUST_PROXY", true),
            login_per_minute: env_u64("JESS_LOGIN_RATE_PER_MINUTE", 5),
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("jess.db")
    }
    pub fn blobs_dir(&self) -> PathBuf {
        self.data_dir.join("blobs")
    }
    pub fn snapshots_dir(&self) -> PathBuf {
        self.data_dir.join("snapshots")
    }
    pub fn mirror_dir(&self) -> PathBuf {
        self.data_dir.join("mirror")
    }
    pub fn engine_config(&self) -> crate::engine::EngineConfig {
        crate::engine::EngineConfig {
            trash_retention_ms: self.trash_retention_days * 86_400_000,
            blob_retention_ms: self.blob_retention_days * 86_400_000,
            max_upload_bytes: self.max_upload_mb << 20,
            ..Default::default()
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
