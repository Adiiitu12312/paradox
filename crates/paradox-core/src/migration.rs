//! Explicit, lossless migration of the former in-tree store.
use crate::{blob::reject_link, repo::Repo};
use anyhow::{Result, ensure, Context};
use std::{fs::{self, OpenOptions}, path::PathBuf};

fn legacy(repo: &Repo) -> Option<PathBuf> {
    repo.common.file_name().filter(|name| *name == ".git")
        .and_then(|_| repo.common.parent()).map(|root| root.join(".paradox"))
}
pub fn require_migrated(repo: &Repo) -> Result<()> {
    if let Some(old) = legacy(repo) {
        ensure!(!old.try_exists()?, "Legacy Paradox store found at {}; run paradox init to migrate it before creating loops", old.display());
    }
    Ok(())
}
pub fn migrate(repo: &Repo) -> Result<()> {
    let Some(old) = legacy(repo).filter(|path| path.exists()) else { return Ok(()) };
    reject_link(&old)?;
    ensure!(old.is_dir(), "Legacy store is not a directory");
    reject_link(&repo.store)?;
    ensure!(!repo.store.exists(), "Both legacy and new Paradox stores exist; preserve both and resolve the migration before continuing");
    let lock_path = old.join("lock");
    reject_link(&lock_path)?;
    let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path)?;
    fs2::FileExt::try_lock_exclusive(&lock).context("Legacy store is in use; stop older Paradox commands before migrating")?;
    let watch_path = old.join("watch.lock");
    reject_link(&watch_path)?;
    let watch = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(watch_path)?;
    fs2::FileExt::try_lock_exclusive(&watch).context("Stop paradox watch before migrating its store")?;
    fs::rename(&old, &repo.store).context("Cannot move the legacy store; its original contents were preserved")?;
    #[cfg(unix)] {
        fs::File::open(&repo.common)?.sync_all()?;
        fs::File::open(old.parent().context("Legacy store has no parent")?)?.sync_all()?;
    }
    // A successful directory rename removes the in-tree copy atomically.
    // Remove only our exact former ignore line, preserving all other bytes.
    let exclude = repo.common.join("info/exclude");
    reject_link(&exclude)?;
    if exclude.is_file() {
        let bytes = fs::read(&exclude)?;
        let kept: Vec<u8> = bytes.split_inclusive(|byte| *byte == b'\n')
            .filter(|line| !matches!(*line, b"/.paradox/\n" | b"/.paradox/\r\n" | b"/.paradox/"))
            .flatten().copied().collect();
        if kept != bytes { crate::blob::atomic_write(&exclude, &kept)?; }
    }
    Ok(())
}
