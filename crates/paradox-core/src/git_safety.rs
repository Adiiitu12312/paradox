//! Shared durable snapshots before explicitly opted-in native Git commands.
//! No UI dependency and no alternate blob/reference insertion implementation.
use crate::{
    repo::Repo,
    store::{LoopStore, NewLoop, file_mode, stamp},
};
use anyhow::{Context, Result, ensure};
use rusqlite::OptionalExtension;
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Component, Path},
};

#[derive(Debug, Default)]
pub struct SafetyReport {
    pub saved: usize,
    pub unchanged: usize,
    pub skipped: usize,
}
struct Snapshot {
    bytes: Vec<u8>,
    deleted: bool,
    mode: Option<u32>,
    state: Option<(i64, u64)>,
}

fn tracked_changes(repo: &Repo) -> Result<Vec<String>> {
    let output = repo.git(&["status", "--porcelain=v2", "-z", "--untracked-files=no"])?;
    let mut entries = output.stdout.split(|byte| *byte == 0);
    let mut paths = BTreeSet::new();
    while let Some(entry) = entries.next() {
        let count = match entry.first() {
            Some(b'1') => 9,
            Some(b'2') => 10,
            Some(b'u') => 11,
            _ => continue,
        };
        let path = entry
            .splitn(count, |byte| *byte == b' ')
            .nth(count - 1)
            .context("invalid tracked status record")?;
        paths.insert(std::str::from_utf8(path)?.to_owned());
        if entry.first() == Some(&b'2') {
            paths.insert(
                std::str::from_utf8(entries.next().context("missing rename source")?)?.to_owned(),
            );
        }
    }
    Ok(paths.into_iter().collect())
}
fn staged(store: &LoopStore, path: &str) -> Result<Snapshot> {
    let output = store.repo.git(&[
        "--literal-pathspecs",
        "ls-files",
        "--stage",
        "-z",
        "--",
        path,
    ])?;
    let entry = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .find_map(|row| {
            let metadata = row.split(|byte| *byte == b'\t').next()?;
            let fields: Vec<_> = metadata.split(|byte| *byte == b' ').collect();
            (fields.len() == 3 && fields[2] == b"0").then_some((fields[0], fields[1]))
        });
    let Some((mode, oid)) = entry else {
        ensure!(
            output.stdout.is_empty(),
            "unmerged index has no stage-0 version"
        );
        return Ok(Snapshot {
            bytes: Vec::new(),
            deleted: true,
            mode: None,
            state: None,
        });
    };
    ensure!(
        matches!(mode, b"100644" | b"100755"),
        "linked/submodule staged paths are skipped"
    );
    let oid = std::str::from_utf8(oid)?;
    let size = store.repo.git(&["cat-file", "-s", oid])?;
    ensure!(
        std::str::from_utf8(&size.stdout)?.trim().parse::<u64>()? <= store.config.size_limit_bytes,
        "staged file exceeds configured loop size limit"
    );
    let output = crate::git::output_bounded(
        store.repo.command(&["cat-file", "blob", oid]),
        None,
        usize::try_from(store.config.size_limit_bytes).unwrap_or(usize::MAX),
    )?;
    ensure!(output.status.success(), "cannot read staged blob");
    Ok(Snapshot {
        bytes: output.stdout,
        deleted: false,
        mode: Some(if mode == b"100755" { 0o755 } else { 0o644 }),
        state: None,
    })
}
fn working(store: &LoopStore, path: &str) -> Result<Snapshot> {
    let mut target = store.repo.root.clone();
    let parts: Vec<_> = Path::new(path).components().collect();
    for (index, part) in parts.iter().enumerate() {
        ensure!(matches!(part, Component::Normal(_)), "invalid safety path");
        target.push(part.as_os_str());
        match fs::symlink_metadata(&target) {
            Ok(metadata) => {
                ensure!(
                    !metadata.file_type().is_symlink(),
                    "linked working paths are skipped"
                );
                if index + 1 != parts.len() {
                    ensure!(metadata.is_dir(), "unsafe safety path parent");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Snapshot {
                    bytes: Vec::new(),
                    deleted: true,
                    mode: None,
                    state: None,
                });
            }
            Err(error) => return Err(error.into()),
        }
    }
    let before = store
        .repo
        .regular_file(path)?
        .context("working path is not a regular file")?;
    ensure!(
        before.len() <= store.config.size_limit_bytes,
        "working file exceeds configured loop size limit"
    );
    let state = stamp(&before)?;
    let mut bytes = Vec::new();
    fs::File::open(&target)?
        .take(store.config.size_limit_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let after = store
        .repo
        .regular_file(path)?
        .context("working file changed type during safety capture")?;
    ensure!(
        state == stamp(&after)? && bytes.len() as u64 == after.len(),
        "working file changed during safety capture"
    );
    Ok(Snapshot {
        bytes,
        deleted: false,
        mode: file_mode(&after),
        state: Some(state),
    })
}
fn save(
    store: &mut LoopStore,
    path: &str,
    source: &str,
    command: &str,
    snapshot: Snapshot,
    pins: &mut Vec<i64>,
) -> Result<bool> {
    ensure!(
        snapshot.bytes.len() as u64 <= store.config.size_limit_bytes,
        "safety file exceeds configured size limit"
    );
    ensure!(
        !snapshot.bytes.iter().take(8192).any(|byte| *byte == 0),
        "binary contents are skipped by safety capture"
    );
    let hash = if snapshot.deleted {
        String::new()
    } else {
        blake3::hash(&snapshot.bytes).to_hex().to_string()
    };
    let latest=store.index.query_row("SELECT blob_hash,deleted,mode FROM loops WHERE worktree=?1 AND path=?2 ORDER BY ts DESC,id DESC LIMIT 1",rusqlite::params![store.repo.worktree,path],|row|Ok((row.get::<_,String>(0)?,row.get::<_,bool>(1)?,row.get::<_,Option<u32>>(2)?))).optional()?;
    if latest.is_some_and(|(old, deleted, mode)| {
        old == hash
            && deleted == snapshot.deleted
            && mode.zip(snapshot.mode).is_none_or(|(a, b)| a == b)
    }) {
        if let Some(state) = snapshot.state {
            store.cache_write(path, state, &hash)?;
        }
        return Ok(false);
    }
    let message = format!("before git {command} ({source})");
    let id = store.save_loop(NewLoop {
        path,
        bytes: &snapshot.bytes,
        hash: &hash,
        state: snapshot.state,
        kind: if snapshot.deleted {
            "deleted"
        } else {
            "safety"
        },
        message: &message,
        auto_label: false,
        deleted: snapshot.deleted,
        pinned: true,
        mode: snapshot.mode,
    })?;
    pins.push(id);
    Ok(true)
}

/// Capture stage 0 first, then working bytes, so the newest per-file loop is
/// the working version under storage pressure. Every new row is pinned until
/// all versions are durable. Existing pins/rows are never changed.
pub fn capture_before(repo: Repo, command: &str) -> Result<SafetyReport> {
    let repo = repo.refreshed_head()?;
    let paths = tracked_changes(&repo)?;
    if paths.is_empty() {
        return Ok(SafetyReport::default());
    }
    let mut store = LoopStore::open(repo)?;
    let mut report = SafetyReport::default();
    let mut pins = Vec::new();
    let before: i64 =
        store
            .index
            .query_row("SELECT COALESCE(MAX(id),0) FROM loops", [], |row| {
                row.get(0)
            })?;
    let result = (|| -> Result<()> {
        for path in paths {
            ensure!(
                store.repo.relative(&store.repo.root.join(&path))? == path,
                "invalid safety path"
            );
            if store.denied(&path) {
                report.skipped += 2;
                store.log(&format!("safety capture skipped denied path {path:?}"));
                continue;
            }
            for source in ["staged", "working"] {
                let snapshot = if source == "staged" {
                    staged(&store, &path)
                } else {
                    working(&store, &path)
                };
                match snapshot.and_then(|snapshot| {
                    save(&mut store, &path, source, command, snapshot, &mut pins)
                }) {
                    Ok(true) => report.saved += 1,
                    Ok(false) => report.unchanged += 1,
                    Err(error) => {
                        report.skipped += 1;
                        store.log(&format!("safety {source} skipped {path:?}: {error:#}"));
                    }
                }
            }
        }
        Ok(())
    })();
    // Release only rows created by this call, still under its existing lock.
    // Do not run GC after unpinning: current snapshots must reach native Git.
    // Include durable rows from a save that reported a later checkpoint error.
    pins.extend(
        store
            .index
            .prepare("SELECT id FROM loops WHERE id>?1 AND worktree=?2 AND pinned=1")?
            .query_map(rusqlite::params![before, store.repo.worktree], |row| {
                row.get::<_, i64>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    );
    pins.sort_unstable();
    pins.dedup();
    store.index.execute_batch("BEGIN IMMEDIATE")?;
    let release = (|| -> Result<()> {
        for id in pins {
            store.pin_loop(id, false)?;
            store
                .index
                .execute("DELETE FROM operation_pins WHERE id=?1", [id])?;
        }
        Ok(())
    })();
    if let Err(error) = release {
        store.index.execute_batch("ROLLBACK")?;
        return Err(error);
    }
    store
        .index
        .execute_batch("COMMIT; PRAGMA wal_checkpoint(TRUNCATE);")?;
    result?;
    Ok(report)
}
