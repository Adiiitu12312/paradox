use crate::repo::Repo;
use crate::store::LoopStore;
use anyhow::{Context, Result, ensure};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

pub enum StagePolicy {
    Auto {
        paths: Vec<std::ffi::OsString>,
        options: Vec<std::ffi::OsString>,
    },
    ExistingIndex,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct CommandRecord {
    pub argv: Vec<String>,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub struct CommitOutcome {
    pub code: i32,
    pub commands: Vec<CommandRecord>,
}

fn commit_command(mut command: std::process::Command, capture: bool) -> Result<CommandRecord> {
    let argv = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    if capture {
        let output = crate::git::output_bounded(command, None, 8_388_608)?;
        Ok(CommandRecord {
            argv,
            exit_code: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    } else {
        let status = command.status()?;
        Ok(CommandRecord {
            argv,
            exit_code: status.code().unwrap_or(1),
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

/// Shared CLI/UI commit orchestration. ExistingIndex never stages unrelated
/// work: Git constructs its index, then mandatory hooks strip marker bytes.
pub fn execute_commit(
    repo: Repo,
    executable: &Path,
    args: Vec<std::ffi::OsString>,
    stage: StagePolicy,
    no_verify: bool,
    capture_output: bool,
) -> Result<CommitOutcome> {
    use std::ffi::OsString;
    let mut commands = Vec::new();
    let log = repo.store.clone();
    if let Err(error) =
        LoopStore::open(repo.clone()).and_then(|mut store| store.capture(&[]).map(|_| ()))
    {
        crate::store::log_at(&log, &format!("fallback scan: {error:#}"));
    }
    let existing_index = matches!(stage, StagePolicy::ExistingIndex);
    if let StagePolicy::Auto { paths, options } = stage {
        let mut add = repo.globals.clone();
        add.extend([OsString::from("add"), OsString::from("-A")]);
        add.extend(options);
        if !paths.is_empty() {
            add.push("--".into());
            add.extend(paths);
        }
        let record = commit_command(crate::git::command(&add), capture_output)?;
        let code = record.exit_code;
        commands.push(record);
        if code != 0 {
            return Ok(CommitOutcome { code, commands });
        }
    }
    let temporary = tempfile::Builder::new()
        .prefix("paradox-commit-hooks-")
        .tempdir()?;
    let hooks = crate::setup::hooks_dir(&repo)?;
    if hooks.is_dir() {
        for entry in fs::read_dir(&hooks)? {
            let entry = entry?;
            if !crate::setup::runnable(&entry.path()) {
                continue;
            }
            let name = entry.file_name();
            if name == "pre-commit" || name == "post-commit" || (no_verify && name == "commit-msg")
            {
                continue;
            }
            let script = crate::setup::hook_script(executable, Some(&entry.path()), None)?;
            let path = temporary.path().join(name);
            fs::write(&path, script)?;
            crate::setup::executable(&path)?;
        }
    }
    for event in ["pre-commit", "post-commit"] {
        let previous = hooks.join(event);
        let previous = crate::setup::runnable(&previous).then_some(previous);
        let previous = if no_verify && event == "pre-commit" {
            None
        } else {
            previous.as_deref()
        };
        let script = crate::setup::hook_script(executable, previous, Some(event))?;
        let path = temporary.path().join(event);
        fs::write(&path, script)?;
        crate::setup::executable(&path)?;
    }
    let hook_option = format!("core.hooksPath={}", temporary.path().display());
    let command = if existing_index {
        let mut command = repo.command(&["-c", &hook_option, "commit"]);
        command.args(args);
        command
    } else {
        let mut all = repo.globals.clone();
        all.extend([
            OsString::from("-c"),
            hook_option.into(),
            OsString::from("commit"),
        ]);
        all.extend(args);
        crate::git::command(&all)
    };
    let record = commit_command(command, capture_output)?;
    let code = record.exit_code;
    commands.push(record);
    if code == 0
        && let Ok(updated) = Repo::discover(&repo.globals)
        && let Err(error) =
            LoopStore::open(updated).and_then(|mut store| store.purge().map(|_| ()))
    {
        crate::store::log_at(&log, &format!("post-commit purge: {error:#}"));
    }
    Ok(CommitOutcome { code, commands })
}

fn index_path(repo: &Repo) -> Result<PathBuf> {
    let output = repo.git(&["rev-parse", "--path-format=absolute", "--git-path", "index"])?;
    Ok(PathBuf::from(
        std::str::from_utf8(&output.stdout)?.trim_end_matches(['\r', '\n']),
    ))
}

/// Stream the body through Git; memory is bounded by two permitted header lines.
fn stripped_blob(repo: &Repo, path: &Path, oid: &str) -> Result<Option<String>> {
    if path
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("json"))
    {
        return Ok(None);
    }
    let mut source = repo
        .command(&["cat-file", "blob", oid])
        .stdout(Stdio::piped())
        .spawn()?;
    let result = (|| -> Result<Option<String>> {
        let mut reader = BufReader::new(source.stdout.take().context("missing blob output")?);
        let mut prefix = Vec::new();
        for line in 0..2 {
            let start = prefix.len();
            Read::by_ref(&mut reader)
                .take(65_537)
                .read_until(b'\n', &mut prefix)?;
            let value = &prefix[start..];
            if value.len() > 65_536 {
                let possible = [
                    b"//>".as_slice(),
                    b"#>",
                    b"--",
                    b";>",
                    b"%>",
                    b"/*>",
                    b"<!-->",
                    b"#!",
                ]
                .iter()
                .any(|p| value.starts_with(p));
                ensure!(
                    !possible,
                    "message line exceeds 64 KiB; shorten it before committing"
                );
                return Ok(None);
            }
            let first = value.strip_suffix(b"\n").unwrap_or(value);
            let first = first.strip_suffix(b"\r").unwrap_or(first);
            if line == 1 || !(first.starts_with(b"#!") || first == b"<?php") {
                break;
            }
        }
        let Some(range) = crate::message::marker_range(path, &prefix) else {
            return Ok(None);
        };
        let mut hash = repo
            .command(&["hash-object", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let write = (|| -> Result<()> {
            let mut input = hash.stdin.take().context("missing hash input")?;
            input.write_all(&prefix[..range.start])?;
            input.write_all(&prefix[range.end..])?;
            std::io::copy(&mut reader, &mut input)?;
            Ok(())
        })();
        if let Err(error) = write {
            let _ = hash.kill();
            let _ = hash.wait();
            return Err(error);
        }
        let output = hash.wait_with_output()?;
        ensure!(
            output.status.success(),
            "Git could not write stripped content"
        );
        ensure!(
            source.wait()?.success(),
            "Git could not read staged content"
        );
        let oid = std::str::from_utf8(&output.stdout)?.trim().to_string();
        ensure!(
            matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid Git object ID"
        );
        Ok(Some(oid))
    })();
    let _ = source.kill();
    let _ = source.wait();
    result
}

/// Native Git lock convention plus an alternate index makes stripping all-or-nothing.
pub fn strip_staged(repo: &Repo) -> Result<usize> {
    let path = index_path(repo)?;
    crate::blob::reject_link(&path)?;
    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    let lock = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .context("Git index is locked; retry after the other Git operation finishes")?;
    let result = (|| -> Result<usize> {
        let parent = path.parent().context("missing index parent")?;
        let mut temporary = tempfile::Builder::new()
            .prefix("paradox-index-")
            .tempfile_in(parent)?;
        if path.exists() {
            std::io::copy(&mut fs::File::open(&path)?, &mut temporary)?;
        }
        temporary.as_file().sync_all()?;
        if temporary.as_file().metadata()?.len() == 0 {
            let status = repo
                .command(&["read-tree", "--empty"])
                .env("GIT_INDEX_FILE", temporary.path())
                .status()?;
            ensure!(status.success(), "could not initialize temporary Git index");
        }
        let output = repo
            .command(&[
                "diff",
                "--cached",
                "--raw",
                "--no-abbrev",
                "-z",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
            ])
            .env("GIT_INDEX_FILE", temporary.path())
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()?;
        ensure!(output.status.success(), "could not read staged changes");
        let mut records = output.stdout.split(|b| *b == 0);
        let mut updates = Vec::new();
        let mut count = 0;
        while let Some(header) = records.next() {
            if header.is_empty() {
                continue;
            }
            let name = records.next().context("invalid staged Git record")?;
            let fields: Vec<_> = std::str::from_utf8(header)?
                .split_ascii_whitespace()
                .collect();
            ensure!(fields.len() == 5, "invalid staged Git header");
            if !matches!(fields[1], "100644" | "100755") {
                continue;
            }
            let name = std::str::from_utf8(name)
                .context("staged paths must be UTF-8 for marker stripping")?;
            if let Some(oid) = stripped_blob(repo, Path::new(name), fields[3])? {
                updates.extend_from_slice(format!("{} {oid}\t{name}\0", fields[1]).as_bytes());
                count += 1;
            }
        }
        if count == 0 {
            return Ok(0);
        }
        let mut update = repo
            .command(&["update-index", "-z", "--index-info"])
            .env("GIT_INDEX_FILE", temporary.path())
            .stdin(Stdio::piped())
            .spawn()?;
        let write = update
            .stdin
            .take()
            .context("missing index input")?
            .write_all(&updates);
        if let Err(error) = write {
            let _ = update.kill();
            let _ = update.wait();
            return Err(error.into());
        }
        ensure!(update.wait()?.success(), "could not stage stripped content");
        crate::fault("before_strip_index_replace");
        // update-index replaced the pathname; the original tempfile handle
        // still refers to its previous inode. Sync the updated index itself.
        let updated = OpenOptions::new()
            .read(true)
            .write(true)
            .open(temporary.path())?;
        if let Ok(meta) = fs::metadata(&path) {
            updated.set_permissions(meta.permissions())?;
        }
        updated.sync_all()?;
        drop(updated);
        // tempfile's persist uses atomic replacement on Windows as well as Unix.
        temporary
            .into_temp_path()
            .persist(&path)
            .map_err(|e| e.error)?;
        #[cfg(unix)]
        fs::File::open(parent)?.sync_all()?;
        Ok(count)
    })();
    // Keep our lock pathname occupied through replacement and directory fsync.
    // Cleanup cannot unlink a lock acquired by a following Git operation.
    drop(lock);
    let _ = fs::remove_file(&lock_path);
    result
}

impl LoopStore {
    /// Keep current dirty paths, pins, unrelated branches and the last N commits.
    /// Other worktrees are purged only when their own post-commit hook runs.
    pub fn purge(&mut self) -> Result<usize> {
        if self.repo.head.is_empty() || self.config.retention_commits == 0 {
            return Ok(0);
        }
        let output = self.repo.git(&[
            "rev-list",
            "--first-parent",
            &format!("--max-count={}", self.config.retention_commits),
            "HEAD",
        ])?;
        let heads: Vec<_> = std::str::from_utf8(&output.stdout)?.lines().collect();
        if heads.len() < self.config.retention_commits {
            return Ok(0);
        }
        let cutoff = heads.last().context("missing retention boundary")?;
        let cutoff_ms: i64 = std::str::from_utf8(
            &self
                .repo
                .git(&["show", "-s", "--format=%ct", cutoff])?
                .stdout,
        )?
        .trim()
        .parse::<i64>()?
        .saturating_mul(1000);
        let mut dirty = self.repo.dirty()?;
        // A retained marker makes Git report a file dirty even when its actual
        // program bytes match HEAD. That alone must not disable retention.
        dirty.retain(|path| {
            let Ok(metadata) = fs::symlink_metadata(self.repo.root.join(path)) else {
                return true;
            };
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() > self.config.size_limit_bytes
                || self.denied(path)
            {
                return true;
            }
            let Ok(bytes) = fs::read(self.repo.root.join(path)) else {
                return true;
            };
            let Some(range) = crate::message::marker_range(Path::new(path), &bytes) else {
                return true;
            };
            let Ok(version) = self.head_version(path) else {
                return true;
            };
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if version
                    .mode()
                    .is_some_and(|mode| mode & 0o100 != metadata.permissions().mode() & 0o100)
                {
                    return true;
                }
            }
            let Ok(head) = self.version_bytes(path, &version) else {
                return true;
            };
            let mut stripped = bytes[..range.start].to_vec();
            stripped.extend_from_slice(&bytes[range.end..]);
            stripped != head
        });
        let rows = self
            .index
            .prepare("SELECT id,path,head_sha,ts FROM loops WHERE worktree=?1 AND pinned=0")?
            .query_map([&self.repo.worktree], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut ancestry = std::collections::HashMap::new();
        let mut remove = Vec::new();
        for (id, path, head, ts) in rows {
            if dirty.contains(&path) || heads.contains(&head.as_str()) {
                continue;
            }
            let older = if head.is_empty() {
                ts < cutoff_ms
            } else if let Some(older) = ancestry.get(&head) {
                *older
            } else {
                let output = self
                    .repo
                    .command(&["merge-base", "--is-ancestor", &head, cutoff])
                    .output()?;
                let older = output.status.success();
                ancestry.insert(head, older);
                older
            };
            if older {
                remove.push(id);
            }
        }
        self.index.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            for id in &remove {
                self.index
                    .execute("DELETE FROM loops WHERE id=?1 AND pinned=0", [id])?;
                crate::fault("during_purge");
            }
            self.index.execute(
                "DELETE FROM blobs WHERE hash NOT IN (SELECT blob_hash FROM loops)",
                [],
            )?;
            self.index.execute("UPDATE meta SET value=(SELECT COALESCE(SUM(stored_size),0) FROM blobs) WHERE key='blob_bytes'", [])?;
            Ok(())
        })();
        if let Err(error) = result {
            self.index.execute_batch("ROLLBACK")?;
            return Err(error);
        }
        self.index.execute_batch("COMMIT")?;
        crate::fault("after_purge_commit");
        self.gc()?;
        Ok(remove.len())
    }
}
