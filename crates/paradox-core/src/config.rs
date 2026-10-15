use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub size_limit_bytes: u64,
    pub store_cap_bytes: u64,
    pub retention_commits: usize,
    pub secrets_denylist: Vec<String>,
    pub safety_loop_commands: Vec<String>,
    pub watch_idle_minutes: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            size_limit_bytes: 2_097_152,
            store_cap_bytes: 524_288_000,
            retention_commits: 3,
            secrets_denylist: vec![
                ".env*".into(),
                "*.pem".into(),
                "*.key".into(),
                "id_rsa*".into(),
            ],
            safety_loop_commands: Vec::new(),
            watch_idle_minutes: 30,
        }
    }
}

impl Config {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("config.toml");
        let config = if path.exists() {
            ensure!(
                !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
                "config must not be a symlink"
            );
            toml::from_str(&std::fs::read_to_string(path)?)?
        } else {
            Self::default()
        };
        ensure!(
            config.size_limit_bytes > 0,
            "file size limit must be positive"
        );
        ensure!(
            config.store_cap_bytes >= 131_072,
            "store cap must allow at least 128 KiB of metadata"
        );
        Ok(config)
    }
}
