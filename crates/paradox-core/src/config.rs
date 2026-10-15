use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub size_limit_bytes: u64,
    pub store_cap_bytes: u64,
    pub retention_commits: usize,
    pub retention_hours: u64,
    pub retention_min_loops: usize,
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
            retention_hours: 24,
            retention_min_loops: 20,
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
    /// Owned commands synchronize this hint after loading configuration.
    /// Disabled pass-through never needs to parse TOML to discover the flag.
    pub fn sync_safety_flag(&self, store: &Path) -> Result<()> {
        if !store.is_dir() { return Ok(()); }
        let flag=store.join("safety_enabled");
        crate::blob::reject_link(&flag)?;
        if self.safety_loop_commands.is_empty() {
            match std::fs::remove_file(&flag) {
                Ok(()) => { #[cfg(unix)] std::fs::File::open(store)?.sync_all()?; }
                Err(error) if error.kind()==std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        } else {
            let unchanged=std::fs::metadata(&flag).is_ok_and(|metadata|metadata.len()==2)
                && std::fs::read(&flag).is_ok_and(|bytes|bytes==b"1\n");
            if !unchanged { crate::blob::atomic_write(&flag,b"1\n")?; }
        }
        Ok(())
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_config_gets_time_and_count_defaults_and_flag_sync_is_idempotent() {
        let directory=tempfile::tempdir().unwrap();let root=directory.path();
        std::fs::write(root.join("config.toml"),"retention_commits=5\nsafety_loop_commands=['reset']\n").unwrap();
        let mut config=Config::load(root).unwrap();assert_eq!(config.retention_hours,24);assert_eq!(config.retention_min_loops,20);assert_eq!(config.retention_commits,5);
        config.sync_safety_flag(root).unwrap();let flag=root.join("safety_enabled");assert_eq!(std::fs::read(&flag).unwrap(),b"1\n");
        let time=std::time::UNIX_EPOCH+std::time::Duration::from_secs(1_000_000);
        std::fs::File::options().write(true).open(&flag).unwrap().set_times(std::fs::FileTimes::new().set_modified(time)).unwrap();let previous=std::fs::metadata(&flag).unwrap().modified().unwrap();
        config.sync_safety_flag(root).unwrap();assert_eq!(std::fs::metadata(&flag).unwrap().modified().unwrap(),previous);
        config.safety_loop_commands.clear();config.sync_safety_flag(root).unwrap();assert!(!flag.exists());
    }
}
