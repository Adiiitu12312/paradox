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
        let paths = self.track_list()?.worktrees.remove(&self.worktree).unwrap_or_default();
        // Treat the list as untrusted input, including after a manual edit.
        for path in &paths {
            ensure!(self.relative(&self.root.join(path))? == *path, "invalid tracked-list path");
        }
        Ok(paths.into_iter().collect())
    }
}

impl LoopStore {
    pub fn track_paths(&mut self, inputs: &[PathBuf], remove: bool) -> Result<usize> {
        ensure!(!inputs.is_empty(), "provide at least one path");
        let selected = inputs.iter().map(|path| self.repo.relative(path)).collect::<Result<Vec<_>>>()?;
        let mut list = self.repo.track_list()?;
        let mut paths: HashSet<String> = list.worktrees.remove(&self.repo.worktree).unwrap_or_default().into_iter().collect();
        let before = paths.len();
        if remove {
            paths.retain(|path| !selected.iter().any(|input| path == input || path.starts_with(&format!("{input}/"))));
        } else {
            // Git enumerates only explicitly selected paths and honors ignore
            // rules. New directories elsewhere in the tree are never walked.
            let mut args = vec!["--literal-pathspecs", "ls-files", "--cached", "--others", "--exclude-standard", "-z", "--"];
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
        ensure!(self.bytes()?.saturating_sub(old_size).saturating_add(bytes.len() as u64 + 4096) <= self.config.store_cap_bytes, "tracked list would exceed the store cap");
        atomic_write(&path, bytes.as_bytes())?;
        self.index.execute("DELETE FROM tracked_cache WHERE worktree=?1", [&self.repo.worktree])?;
        Ok(changed)
    }
}
