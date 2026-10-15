use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;

use crate::blob::{atomic_write, reject_link};
use crate::repo::Repo;
use crate::store::LoopStore;

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct TrackedPaths {
    worktrees: BTreeMap<String, Vec<String>>,
}

impl Repo {
    fn track_list(&self) -> Result<TrackedPaths> {
        reject_link(&self.store)?;
        let path = self.store.join("tracked.toml");
        reject_link(&path)?;
        match fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TrackedPaths::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn extra_tracked(&self) -> Result<HashSet<String>> {
        let paths = self
            .track_list()?
            .worktrees
            .remove(&self.worktree)
            .unwrap_or_default();
        // Treat the list as untrusted input, including after a manual edit.
        for path in &paths {
            ensure!(
                self.relative(&self.root.join(path))? == *path,
                "invalid tracked-list path"
            );
        }
        Ok(paths.into_iter().collect())
    }
}

impl LoopStore {
    fn apply_git_renames(&self, renames: &[(String, String)]) -> Result<()> {
        if renames.is_empty() {
            return Ok(());
        }
        self.index.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            for (old, new) in renames {
                ensure!(
                    self.repo.relative(&self.repo.root.join(old))? == *old
                        && self.repo.relative(&self.repo.root.join(new))? == *new,
                    "invalid Git rename paths"
                );
                if self.denied(old) || self.denied(new) || old == new {
                    continue;
                }
                self.index.execute(
                    "UPDATE loops SET path=?1 WHERE worktree=?2 AND path=?3",
                    rusqlite::params![new, self.repo.worktree, old],
                )?;
                self.index.execute(
                    "DELETE FROM stat_cache WHERE worktree=?1 AND path IN (?2,?3)",
                    rusqlite::params![self.repo.worktree, old, new],
                )?;
                self.index.execute(
                    "DELETE FROM tracked_cache WHERE worktree=?1 AND path=?2",
                    rusqlite::params![self.repo.worktree, old],
                )?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => self.index.execute_batch("COMMIT")?,
            Err(error) => {
                self.index.execute_batch("ROLLBACK")?;
                return Err(error);
            }
        }
        Ok(())
    }

    pub(crate) fn refresh_staged_renames(&self) -> Result<()> {
        let key = format!("rename_index/{}", self.repo.worktree);
        let signature = self.repo.index_signature();
        if let Some(signature) = &signature {
            let previous =
                self.index
                    .query_row("SELECT value FROM meta WHERE key=?1", [&key], |row| {
                        row.get::<_, String>(0)
                    });
            if previous.is_ok_and(|previous| previous == *signature) {
                return Ok(());
            }
        }
        match self.repo.staged_renames() {
            Ok(renames) => self.apply_git_renames(&renames)?,
            Err(error) => self.log(&format!("staged rename detection skipped: {error:#}")),
        }
        if let Some(signature) = self.repo.index_signature() {
            self.index.execute("INSERT INTO meta VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", rusqlite::params![key, signature])?;
        }
        Ok(())
    }

    pub fn refresh_git_changes(&mut self) -> Result<Option<HashSet<String>>> {
        let key = format!("tracked_scan/{}", self.repo.worktree);
        let signature = self.repo.index_signature();
        let previous = self
            .index
            .query_row("SELECT value FROM meta WHERE key=?1", [&key], |row| {
                row.get::<_, String>(0)
            });
        if let (Some(signature), Ok(previous)) = (&signature, previous)
            && *signature == previous
        {
            return Ok(None);
        }
        let changes = self.repo.changes()?;
        self.apply_git_renames(&changes.renames)?;
        if let Some(signature) = self.repo.index_signature() {
            self.index.execute("INSERT INTO meta VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", rusqlite::params![key, signature])?;
            self.index.execute("INSERT INTO meta VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", rusqlite::params![format!("rename_index/{}", self.repo.worktree), signature])?;
        }
        Ok(Some(changes.dirty))
    }

    pub fn track_paths(&mut self, inputs: &[PathBuf], remove: bool) -> Result<usize> {
        ensure!(!inputs.is_empty(), "provide at least one path");
        let selected = inputs
            .iter()
            .map(|path| self.repo.relative(path))
            .collect::<Result<Vec<_>>>()?;
        let mut list = self.repo.track_list()?;
        let mut paths: HashSet<String> = list
            .worktrees
            .remove(&self.repo.worktree)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let before = paths.len();
        if remove {
            paths.retain(|path| {
                !selected
                    .iter()
                    .any(|input| path == input || path.starts_with(&format!("{input}/")))
            });
        } else {
            // Git enumerates only explicitly selected paths and honors ignore
            // rules. New directories elsewhere in the tree are never walked.
            let mut args = vec![
                "--literal-pathspecs",
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
            ];
            args.extend(selected.iter().map(String::as_str));
            let out = self.repo.git(&args)?;
            for path in out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
                let path = std::str::from_utf8(path)?;
                if !self.denied(path) && self.repo.regular_file(path)?.is_some() {
                    paths.insert(path.into());
                }
            }
        }
        let changed = paths.len().abs_diff(before);
        let mut paths: Vec<_> = paths.into_iter().collect();
        paths.sort();
        list.worktrees.insert(self.repo.worktree.clone(), paths);
        let bytes = toml::to_string(&list)?;
        let path = self.repo.store.join("tracked.toml");
        let old_size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        ensure!(
            self.bytes()?
                .saturating_sub(old_size)
                .saturating_add(bytes.len() as u64 + 4096)
                <= self.config.store_cap_bytes,
            "tracked list would exceed the store cap"
        );
        atomic_write(&path, bytes.as_bytes())?;
        self.index.execute(
            "DELETE FROM tracked_cache WHERE worktree=?1",
            [&self.repo.worktree],
        )?;
        Ok(changed)
    }
}
