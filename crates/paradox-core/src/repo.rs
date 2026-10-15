use anyhow::{Context, Result, bail, ensure};
use std::collections::{BTreeSet, HashSet};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::Output;

#[derive(Clone, Debug)]
pub struct Repo {
    pub globals: Vec<OsString>,
    pub root: PathBuf,
    pub common: PathBuf,
    pub git_dir: PathBuf,
    pub store: PathBuf,
    pub prefix: PathBuf,
    pub worktree: String,
    pub head: String,
}

pub struct Changes {
    pub dirty: HashSet<String>,
    pub renames: Vec<(String, String)>,
}

fn rename_records(output: &[u8]) -> Result<Vec<(String, String)>> {
    let mut entries = output.split(|byte| *byte == 0);
    let mut renames = Vec::new();
    while let Some(status) = entries.next() {
        let status = status.trim_ascii();
        if status.is_empty() {
            continue;
        }
        let from = entries.next().context("invalid Git name-status record")?;
        if matches!(status.first(), Some(b'R' | b'C')) {
            let to = entries.next().context("invalid Git rename record")?;
            if status.first() == Some(&b'R') {
                renames.push((
                    std::str::from_utf8(from)?.into(),
                    std::str::from_utf8(to)?.into(),
                ));
            }
        }
    }
    Ok(renames)
}

impl Repo {
    /// Refresh mutable HEAD for a long-lived UI session, retaining the rooted
    /// repository/worktree identity and without consulting process cwd anew.
    pub fn refreshed_head(mut self) -> Result<Self> {
        let output = crate::git::output_bounded(
            self.command(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]),
            None,
            65_536,
        )?;
        if output.status.code() == Some(1) {
            self.head.clear();
        } else {
            ensure!(output.status.success(), "Cannot refresh current repository HEAD: {}", String::from_utf8_lossy(&output.stderr));
            self.head = std::str::from_utf8(&output.stdout)?.trim().to_owned();
        }
        Ok(self)
    }

    pub fn discover(globals: &[OsString]) -> Result<Self> {
        let mut args = globals.to_vec();
        args.extend(
            [
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-common-dir",
                "--show-prefix",
                "--absolute-git-dir",
                "HEAD",
            ]
            .map(OsString::from),
        );
        let output = crate::git::command(&args)
            .output()
            .context("could not run Git")?;
        let text = std::str::from_utf8(&output.stdout)
            .context("repository paths must be UTF-8 for loop storage")?;
        let lines: Vec<_> = text.lines().collect();
        ensure!(
            matches!(lines.len(), 4 | 5),
            "loops require unambiguous Git repository paths without line breaks"
        );
        let root = PathBuf::from(lines[0]);
        let common = PathBuf::from(lines[1]);
        ensure!(
            root.is_absolute()
                && common.is_absolute()
                && common.is_dir()
                && Path::new(lines[3]).is_absolute(),
            "Git returned invalid repository paths"
        );
        let store = common.join("paradox");
        let worktree = blake3::hash(root.as_os_str().as_encoded_bytes())
            .to_hex()
            .to_string();
        let head = lines
            .get(4)
            .filter(|s| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .copied()
            .unwrap_or_default()
            .to_string();
        Ok(Self {
            globals: globals.to_vec(),
            root,
            common,
            git_dir: PathBuf::from(lines[3]),
            store,
            prefix: PathBuf::from(lines[2]),
            worktree,
            head,
        })
    }

    pub fn command(&self, args: &[&str]) -> std::process::Command {
        // -C was already applied during discovery. Run relative to the root and
        // retain other Git options (including -c) using a final absolute -C.
        let mut all = self.globals.clone();
        all.extend([OsString::from("-C"), self.root.as_os_str().to_owned()]);
        all.push(format!("--git-dir={}", self.git_dir.display()).into());
        all.push(format!("--work-tree={}", self.root.display()).into());
        all.extend(args.iter().map(OsString::from));
        crate::git::command(&all)
    }

    pub fn git(&self, args: &[&str]) -> Result<Output> {
        let result = self.command(args).output()?;
        ensure!(
            result.status.success(),
            "Git operation failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        Ok(result)
    }

    pub fn index_signature(&self) -> Option<String> {
        if std::env::var_os("GIT_INDEX_FILE").is_some() {
            return None;
        }
        let meta = std::fs::metadata(self.git_dir.join("index")).ok()?;
        let time = meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        let extra = match std::fs::symlink_metadata(self.store.join("tracked.toml")) {
            Ok(meta) if meta.is_file() => format!(
                "{}:{}",
                meta.modified()
                    .ok()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_nanos(),
                meta.len()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => "none".into(),
            _ => return None,
        };
        Some(format!("{time}:{}:{extra}", meta.len()))
    }

    pub fn relative(&self, input: &Path) -> Result<String> {
        let path = if input.is_absolute() {
            if let Ok(relative) = input.strip_prefix(&self.root) {
                relative.to_path_buf()
            } else {
                // Editors may spell the same root through /var vs /private/var
                // or Windows short names. Resolve only the root alias; preserve
                // every component below it so symlink-file guards still apply.
                let canonical = std::fs::canonicalize(&self.root)?;
                let alias = input
                    .ancestors()
                    .skip(1)
                    .filter(|ancestor| {
                        std::fs::canonicalize(ancestor).is_ok_and(|p| p == canonical)
                    })
                    .last()
                    .context("file is outside this working tree")?;
                input.strip_prefix(alias)?.to_path_buf()
            }
        } else {
            self.prefix.join(input)
        };
        let mut clean = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(p) => clean.push(p),
                Component::CurDir => {}
                Component::ParentDir => ensure!(clean.pop(), "file is outside this working tree"),
                _ => bail!("invalid file path"),
            }
        }
        let path = clean
            .to_str()
            .context("loop paths must be UTF-8")?
            .to_owned();
        #[cfg(windows)]
        let path = path.replace('\\', "/");
        ensure!(
            !path.is_empty() && !path.contains('\0'),
            "invalid empty file path"
        );
        Ok(path)
    }

    pub fn tracked(&self, selected: &[String]) -> Result<HashSet<String>> {
        let mut tracked = self.git_tracked(selected)?;
        tracked.extend(self.extra_tracked()?.into_iter().filter(|path| {
            selected.is_empty()
                || selected
                    .iter()
                    .any(|input| path == input || path.starts_with(&format!("{input}/")))
        }));
        Ok(tracked)
    }

    fn git_tracked(&self, selected: &[String]) -> Result<HashSet<String>> {
        let mut args = vec!["--literal-pathspecs", "ls-files", "--cached", "-z", "--"];
        args.extend(selected.iter().map(String::as_str));
        let output = self.git(&args)?;
        let tracked: HashSet<String> = output
            .stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .filter_map(|p| std::str::from_utf8(p).ok().map(str::to_owned))
            .collect();
        Ok(tracked)
    }

    /// Root-relative tracked candidates, without discovering untracked files.
    pub fn timeline_paths(&self) -> Result<Vec<String>> {
        let mut paths: BTreeSet<_> = self.git_changes()?.dirty.into_iter().collect();
        let cached = self.git_tracked(&[])?;
        paths.extend(
            self.extra_tracked()?
                .into_iter()
                .filter(|path| !cached.contains(path)),
        );

        // Read current HEAD rather than the discovery snapshot: a Repo may be
        // reused after a native Git commit. Unborn repositories have no diff.
        let head = self
            .command(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
            .output()?;
        if head.status.code() == Some(1) {
            return Ok(paths.into_iter().collect());
        }
        ensure!(
            head.status.success(),
            "Git HEAD lookup failed: {}",
            String::from_utf8_lossy(&head.stderr)
        );
        let head = std::str::from_utf8(&head.stdout)?.trim();
        let commits = self.git(&[
            "--no-lazy-fetch",
            "rev-list",
            "--first-parent",
            "--max-count=4",
            head,
        ])?;
        let boundary = std::str::from_utf8(&commits.stdout)?
            .lines()
            .last()
            .context("missing timeline boundary")?;
        let changed = self.git(&[
            "--no-lazy-fetch",
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            boundary,
            "--",
        ])?;
        paths.extend(
            changed
                .stdout
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .filter_map(|path| std::str::from_utf8(path).ok().map(str::to_owned)),
        );
        Ok(paths.into_iter().collect())
    }

    pub fn dirty(&self) -> Result<HashSet<String>> {
        Ok(self.changes()?.dirty)
    }

    /// Index/HEAD comparison only; never a working-tree stat scan on saves.
    pub fn staged_renames(&self) -> Result<Vec<(String, String)>> {
        let output = self.git(&[
            "--no-lazy-fetch",
            "diff",
            "--cached",
            "--name-status",
            "-z",
            "--find-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--",
        ])?;
        rename_records(&output.stdout)
    }

    pub fn previous_path(&self, path: &str, commit: &str) -> Result<String> {
        let mut path = path.to_owned();
        for (old, new) in self.staged_renames()? {
            if new == path {
                path = old;
            }
        }
        if commit != self.head {
            let range = format!("{commit}..HEAD");
            let output = self.git(&[
                "--no-lazy-fetch",
                "--literal-pathspecs",
                "log",
                "--follow",
                "--no-show-signature",
                "--no-notes",
                "--no-decorate",
                "--format=",
                "--name-status",
                "-z",
                "--find-renames",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                &range,
                "--",
                &path,
            ])?;
            for (old, new) in rename_records(&output.stdout)? {
                if new == path {
                    path = old;
                }
            }
        }
        Ok(path)
    }

    pub fn changes(&self) -> Result<Changes> {
        let mut changes = self.git_changes()?;
        changes.dirty.extend(self.extra_tracked()?);
        Ok(changes)
    }

    fn git_changes(&self) -> Result<Changes> {
        let output = self.git(&["status", "--porcelain=v2", "-z", "--untracked-files=no"])?;
        let mut dirty = HashSet::new();
        let mut renames = Vec::new();
        let mut entries = output.stdout.split(|b| *b == 0);
        while let Some(entry) = entries.next() {
            let fields = match entry.first() {
                Some(b'1') => 9,
                Some(b'2') => 10,
                Some(b'u') => 11,
                _ => continue,
            };
            if let Some(path) = entry
                .splitn(fields, |b| *b == b' ')
                .nth(fields - 1)
                .and_then(|p| std::str::from_utf8(p).ok())
            {
                dirty.insert(path.to_string());
            }
            if entry.first() == Some(&b'2')
                && let Some(old) = entries.next()
                && entry
                    .splitn(10, |byte| *byte == b' ')
                    .nth(8)
                    .is_some_and(|score| score.starts_with(b"R"))
                && let (Ok(old), Some(new)) = (
                    std::str::from_utf8(old),
                    entry
                        .splitn(10, |byte| *byte == b' ')
                        .nth(9)
                        .and_then(|path| std::str::from_utf8(path).ok()),
                )
            {
                renames.push((old.into(), new.into()));
            }
        }
        Ok(Changes { dirty, renames })
    }

    /// Refuse symlinks in any component, including directory links outside Git.
    pub fn regular_file(&self, path: &str) -> Result<Option<std::fs::Metadata>> {
        self.regular_file_cached(path, &mut HashSet::new())
    }

    /// Stat only supplied tracked paths. Windows and macOS metadata calls benefit
    /// from bounded I/O concurrency; every worker verifies directory components.
    pub fn scan_metadata(
        &self,
        paths: &[String],
    ) -> Result<Vec<Result<Option<std::fs::Metadata>>>> {
        let workers = if cfg!(windows) && paths.len() >= 512 {
            8
        } else if cfg!(target_os = "macos") && paths.len() >= 512 {
            4
        } else {
            1
        };
        let chunk_size = paths.len().div_ceil(workers).max(1);
        std::thread::scope(|scope| {
            let handles: Vec<_> = paths
                .chunks(chunk_size)
                .map(|chunk| {
                    scope.spawn(move || {
                        let mut checked = HashSet::new();
                        chunk
                            .iter()
                            .map(|path| self.regular_file_cached(path, &mut checked))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            let mut metadata = Vec::with_capacity(paths.len());
            for handle in handles {
                metadata.extend(
                    handle
                        .join()
                        .map_err(|_| anyhow::anyhow!("stat worker failed"))?,
                );
            }
            Ok(metadata)
        })
    }

    pub fn regular_file_cached(
        &self,
        path: &str,
        checked_dirs: &mut HashSet<PathBuf>,
    ) -> Result<Option<std::fs::Metadata>> {
        let mut file = self.root.clone();
        let parts: Vec<_> = Path::new(path).components().collect();
        for (index, part) in parts.iter().enumerate() {
            ensure!(
                matches!(part, Component::Normal(_)),
                "invalid stored file path"
            );
            file.push(part.as_os_str());
            if index < parts.len() - 1 && checked_dirs.contains(&file) {
                continue;
            }
            match std::fs::symlink_metadata(&file) {
                Ok(m) => {
                    if m.file_type().is_symlink() {
                        return Ok(None);
                    }
                    if index == parts.len() - 1 {
                        return Ok(m.is_file().then_some(m));
                    }
                    if !m.is_dir() {
                        return Ok(None);
                    }
                    checked_dirs.insert(file.clone());
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(None)
    }
}
