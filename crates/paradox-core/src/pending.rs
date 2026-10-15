//! Contended save handoff. Markers contain paths, never file contents.
use crate::{
    blob::{atomic_write, directory, reject_link},
    repo::Repo,
    store::LoopStore,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
fn unique() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
#[derive(Serialize, Deserialize)]
struct Pending {
    root: PathBuf,
    git_dir: PathBuf,
    files: Vec<PathBuf>,
}

pub fn enqueue(repo: &Repo, files: &[PathBuf]) -> Result<()> {
    let mut selected = Vec::new();
    for file in files {
        let path = if file.is_absolute() {
            file.clone()
        } else {
            repo.root.join(&repo.prefix).join(file)
        };
        selected.push(PathBuf::from(repo.relative(&path)?));
    }
    let pending = repo.store.join("pending");
    directory(&pending)?;
    atomic_write(
        &pending.join(format!("{}.json", unique())),
        &serde_json::to_vec(&Pending {
            root: repo.root.clone(),
            git_dir: repo.git_dir.clone(),
            files: selected,
        })?,
    )?;
    Ok(())
}
fn available(store: &Path) -> Result<bool> {
    let pending = store.join("pending");
    reject_link(&pending)?;
    if !pending.is_dir() {
        return Ok(false);
    }
    Ok(fs::read_dir(pending)?.any(|entry| {
        entry.is_ok_and(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry.path().extension().is_some_and(|ext| ext == "json")
        })
    }))
}
fn drain(store: &mut LoopStore) -> Result<bool> {
    if !available(&store.repo.store)? {
        return Ok(false);
    }
    let pending = store.repo.store.join("pending");
    let mut groups = std::collections::BTreeMap::<(PathBuf, PathBuf), Vec<PathBuf>>::new();
    let mut claimed = Vec::new();
    for entry in fs::read_dir(&pending)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || entry.path().extension().is_none_or(|ext| ext != "json")
        {
            continue;
        }
        let target = pending.join(format!("processing-{}.json", unique()));
        match fs::rename(entry.path(), &target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
        let parsed = (|| -> Result<Pending> {
            ensure!(
                fs::metadata(&target)?.len() <= 65_536,
                "pending path marker is too large"
            );
            Ok(serde_json::from_slice(&fs::read(&target)?)?)
        })();
        match parsed {
            Ok(marker) => {
                let files = groups.entry((marker.root, marker.git_dir)).or_default();
                // Empty means a requested tracked-set scan; never narrow it later.
                if marker.files.is_empty() {
                    files.clear();
                    files.push(PathBuf::new());
                } else if !files.iter().any(|file| file.as_os_str().is_empty()) {
                    files.extend(marker.files);
                }
            }
            Err(error) => store.log(&format!("invalid pending marker: {error:#}")),
        }
        claimed.push(target);
    }
    let original = store.repo.clone();
    for ((root, git_dir), mut files) in groups {
        let result = (|| -> Result<()> {
            let repo = Repo::discover(&[
                "-C".into(),
                root.as_os_str().to_owned(),
                format!("--git-dir={}", git_dir.display()).into(),
                format!("--work-tree={}", root.display()).into(),
            ])?;
            ensure!(
                repo.store == original.store,
                "pending marker points outside the shared store"
            );
            for file in &files {
                if !file.as_os_str().is_empty() {
                    ensure!(
                        !file.is_absolute()
                            && repo.relative(&repo.root.join(file))? == file.to_string_lossy(),
                        "invalid pending path"
                    );
                }
            }
            if files.iter().any(|file| file.as_os_str().is_empty()) {
                files.clear();
            }
            files.sort();
            files.dedup();
            store.repo = repo;
            store.capture(&files)?;
            Ok(())
        })();
        if let Err(error) = result {
            store.log(&format!("pending capture: {error:#}"));
        }
    }
    store.repo = original;
    for path in &claimed {
        fs::remove_file(path)?;
    }
    #[cfg(unix)]
    fs::File::open(&pending)?.sync_all()?;
    Ok(!claimed.is_empty())
}

pub(crate) fn finish(store: &mut LoopStore) {
    let result = (|| -> Result<()> {
        loop {
            while drain(store)? {}
            // SQLite may delete its WAL/SHM files while its last connection
            // closes. Finish that work while still owning the store lock;
            // otherwise a Windows successor can see pending-delete files and
            // fail to open the store with AccessDenied.
            let index =
                std::mem::replace(&mut store.index, rusqlite::Connection::open_in_memory()?);
            index.close().map_err(|(_, error)| error)?;
            fs2::FileExt::unlock(&store._lock)?;
            // A writer publishes before its final nonblocking lock attempt.
            // Check again AFTER unlocking to close the empty-queue/release race.
            if !available(&store.repo.store)? {
                return Ok(());
            }
            match fs2::FileExt::try_lock_exclusive(&store._lock) {
                Ok(()) => {
                    for name in ["index.sqlite3", "index.sqlite3-wal", "index.sqlite3-shm"] {
                        reject_link(&store.repo.store.join(name))?;
                    }
                    let index = rusqlite::Connection::open(store.repo.store.join("index.sqlite3"))?;
                    index.busy_timeout(std::time::Duration::from_secs(5))?;
                    index.execute_batch(
                        "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
                    )?;
                    store.index = index;
                }
                Err(error)
                    if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
                {
                    return Ok(());
                } // The successor owns draining.
                Err(error) => return Err(error.into()),
            }
        }
    })();
    if let Err(error) = result {
        store.log(&format!("pending handoff: {error:#}"));
        let _ = fs2::FileExt::unlock(&store._lock);
    }
}
