use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Local, NaiveTime, SecondsFormat, TimeZone, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::blob::{BlobStore, atomic_write, reject_link};
use crate::store::{Loop, LoopStore, NewLoop, file_mode, now, stamp};

#[derive(Clone, Debug)]
enum Source {
    Loop(String),
    Git(String),
    Working,
    Absent,
}

#[derive(Clone, Debug)]
pub struct Version {
    path: String,
    source: Source,
    pub id: Option<i64>,
    pub ts: i64,
    pub label: String,
    pub deleted: bool,
    mode: Option<u32>,
}

impl Version {
    fn from_loop(record: Loop) -> Self {
        Self { path: record.path, source: Source::Loop(record.blob_hash), id: Some(record.id), ts: record.ts,
            label: format!("loop {}", record.id), deleted: record.deleted, mode: record.mode }
    }
}

#[derive(Clone, Debug)]
pub struct RewindPlan {
    pub path: String,
    pub version: Version,
}

#[derive(Default)]
pub struct RewindPreview {
    pub plans: Vec<RewindPlan>,
    pub skipped: Vec<(String, String)>,
}

struct Working {
    bytes: Vec<u8>,
    state: Option<(i64, u64)>,
    mode: Option<u32>,
    deleted: bool,
}

#[derive(Deserialize, Serialize)]
struct WritingMarker {
    worktree: String,
    pid: u32,
    path: String,
    expected_hash: String,
    deleted: bool,
    started_ms: i64,
}

pub fn absolute_time(ts: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ts)
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_else(|| format!("{ts} ms Unix"))
}

pub fn relative_time(ts: i64) -> String {
    let seconds = (Utc::now().timestamp_millis().saturating_sub(ts) / 1000).max(0);
    if seconds < 60 { format!("{seconds}s ago") }
    else if seconds < 3600 { format!("{}m ago", seconds / 60) }
    else if seconds < 86400 { format!("{}h ago", seconds / 3600) }
    else { format!("{}d ago", seconds / 86400) }
}

/// Most recent local occurrence of HH:MM. Ambiguous/nonexistent DST times fail.
pub fn clock_time(value: &str) -> Result<i64> {
    ensure!(value.len() == 5 && value.as_bytes()[2] == b':', "use HH:MM in local time");
    let time = NaiveTime::parse_from_str(value, "%H:%M").context("invalid HH:MM")?;
    let current = Local::now();
    let mut date = current.date_naive();
    let mut target = Local.from_local_datetime(&date.and_time(time)).single().context("ambiguous or nonexistent local time; choose a loop ID instead")?;
    if target > current {
        date = date.pred_opt().context("date out of range")?;
        target = Local.from_local_datetime(&date.and_time(time)).single().context("ambiguous or nonexistent local time; choose a loop ID instead")?;
    }
    Ok(target.timestamp_millis())
}

fn relative_target(value: &str) -> Result<Option<i64>> {
    let Some((unit, digits)) = value.chars().next_back().map(|last| (last, &value[..value.len() - last.len_utf8()])) else { return Ok(None) };
    let multiplier: i64 = match unit { 's' => 1000, 'm' => 60000, 'h' => 3600000, 'd' => 86400000, _ => return Ok(None) };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) { return Ok(None); }
    let amount = digits.parse::<i64>().context("relative time is too large")?;
    let age = amount.checked_mul(multiplier).context("relative time is too large")?;
    Ok(Some(now()?.checked_sub(age).context("relative time is out of range")?))
}

impl LoopStore {
    fn marker_path(&self) -> PathBuf {
        self.repo.store.join(format!("writing-{}.toml", self.repo.worktree))
    }

    fn remove_writing_marker(&self) -> Result<()> {
        let path = self.marker_path();
        reject_link(&path)?;
        match fs::remove_file(path) {
            Ok(()) => {
                #[cfg(unix)]
                File::open(&self.repo.store)?.sync_all()?;
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) fn recover_writing_marker(&self) -> Result<()> {
        let path = self.marker_path();
        reject_link(&path)?;
        if let Ok(meta) = fs::metadata(&path) { ensure!(meta.len() <= 65536, "writing marker exceeds its size limit"); }
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let marker: WritingMarker = toml::from_str(&text)?;
        if marker.worktree != self.repo.worktree { return Ok(()); }
        ensure!(!self.denied(&marker.path) && marker.expected_hash.len() == 64 && marker.expected_hash.bytes().all(|byte| byte.is_ascii_hexdigit()), "invalid writing marker");
        let result = (|| -> Result<()> {
            let exists = self.valid_target(&marker.path)?;
            if marker.deleted {
                if !exists { self.index.execute("DELETE FROM stat_cache WHERE worktree=?1 AND path=?2", params![self.repo.worktree, marker.path])?; }
                return Ok(());
            }
            if !exists { return Ok(()); }
            let meta = self.repo.regular_file(&marker.path)?.context("file changed type")?;
            let state = stamp(&meta)?;
            let cached: Option<(i64, i64, String)> = self.index.query_row("SELECT mtime_ns,size,last_hash FROM stat_cache WHERE worktree=?1 AND path=?2", params![self.repo.worktree, marker.path], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()?;
            if cached.is_some_and(|(mtime, size, hash)| mtime == state.0 && size >= 0 && size as u64 == state.1 && hash == marker.expected_hash) { return Ok(()); }
            let current = self.read_working(&marker.path)?;
            if !current.deleted && blake3::hash(&current.bytes).to_hex().as_str() == marker.expected_hash {
                self.cache_write(&marker.path, current.state.context("missing file state")?, &marker.expected_hash)?;
            }
            Ok(())
        })();
        // A marker is a one-shot recovery record. Keeping it indefinitely would
        // wrongly suppress a later external edit back to the same bytes.
        self.remove_writing_marker()?;
        result
    }

    fn commit_time(&self, commit: &str) -> Result<i64> {
        let output = self.repo.git(&["show", "-s", "--format=%ct", commit])?;
        let seconds = std::str::from_utf8(&output.stdout)?.trim().parse::<i64>()?;
        seconds.checked_mul(1000).context("commit time is out of range")
    }

    fn git_version(&self, path: &str, commit: &str, ts: i64, label: &str) -> Result<Version> {
        if commit.is_empty() {
            return Ok(Version { path: path.into(), source: Source::Absent, id: None, ts, label: label.into(), deleted: true, mode: None });
        }
        let output = self.repo.git(&["--literal-pathspecs", "ls-tree", "-z", commit, "--", path])?;
        let entry = output.stdout.split(|byte| *byte == 0).find(|entry| !entry.is_empty());
        let Some(entry) = entry else {
            return Ok(Version { path: path.into(), source: Source::Absent, id: None, ts, label: label.into(), deleted: true, mode: None });
        };
        let tab = entry.iter().position(|byte| *byte == b'\t').context("invalid Git tree record")?;
        ensure!(&entry[tab + 1..] == path.as_bytes(), "Git returned a different file");
        let fields: Vec<_> = std::str::from_utf8(&entry[..tab])?.split(' ').collect();
        ensure!(fields.len() == 3 && fields[1] == "blob" && matches!(fields[0], "100644" | "100755"), "only regular files can be compared or rewound");
        ensure!(matches!(fields[2].len(), 40 | 64) && fields[2].bytes().all(|byte| byte.is_ascii_hexdigit()), "invalid Git object ID");
        Ok(Version { path: path.into(), source: Source::Git(fields[2].into()), id: None, ts, label: label.into(), deleted: false, mode: Some(if fields[0] == "100755" { 0o755 } else { 0o644 }) })
    }

    pub fn head_version(&self, path: &str) -> Result<Version> {
        let ts = if self.repo.head.is_empty() { 0 } else { self.commit_time(&self.repo.head)? };
        self.git_version(path, &self.repo.head, ts, "HEAD")
    }

    fn commit_before(&self, at: i64) -> Result<Option<(String, i64)>> {
        if self.repo.head.is_empty() { return Ok(None); }
        let time = DateTime::<Utc>::from_timestamp_millis(at).context("time is out of range")?.to_rfc3339();
        let before = format!("--before={time}");
        let output = self.repo.git(&["rev-list", "-1", &before, "HEAD"])?;
        let commit = std::str::from_utf8(&output.stdout)?.trim();
        if commit.is_empty() { return Ok(None); }
        Ok(Some((commit.into(), self.commit_time(commit)?)))
    }

    fn at_time(&self, path: &str, at: i64, commit: &Option<(String, i64)>) -> Result<Version> {
        let record = self.loops(path)?.into_iter().find(|record| record.ts <= at);
        if let Some(record) = record {
            if commit.as_ref().is_none_or(|(_, ts)| record.ts >= *ts) { return Ok(Version::from_loop(record)); }
        }
        if let Some((commit, ts)) = commit { return self.git_version(path, commit, *ts, &format!("commit {}", &commit[..commit.len().min(12)])); }
        bail!("no recorded version at or before that time for {path:?}; choose HEAD or a loop ID")
    }

    pub fn resolve(&self, path: &str, selector: &str) -> Result<Version> {
        ensure!(!selector.is_empty(), "empty version selector");
        if selector == "HEAD" { return self.head_version(path); }
        if selector == "working" {
            let current = self.read_working(path)?;
            return Ok(Version { path: path.into(), source: Source::Working, id: None, ts: now()?, label: "working".into(), deleted: current.deleted, mode: current.mode });
        }
        if let Some(at) = relative_target(selector)? { return self.at_time(path, at, &self.commit_before(at)?); }
        let loops = self.loops(path)?;
        if selector == "undo" {
            return loops.into_iter().find(|record| record.kind == "safety").map(Version::from_loop).context("no safety loop to undo to");
        }
        if !selector.is_empty() && selector.bytes().all(|byte| byte.is_ascii_digit()) {
            let id = selector.parse::<i64>().context("loop ID is out of range")?;
            return loops.into_iter().find(|record| record.id == id).map(Version::from_loop).context("loop ID does not belong to this file and working tree");
        }
        let mut matches = loops.into_iter().filter(|record| record.message.contains(selector));
        let record = matches.next().context("no matching loop message")?;
        ensure!(matches.next().is_none(), "ambiguous loop message; choose an ID");
        Ok(Version::from_loop(record))
    }

    pub fn latest_version(&self, path: &str) -> Result<Version> {
        match self.loops(path)?.into_iter().next() {
            Some(record) => Ok(Version::from_loop(record)),
            None => self.head_version(path),
        }
    }

    fn valid_target(&self, path: &str) -> Result<bool> {
        ensure!(self.repo.relative(&self.repo.root.join(path))? == path, "invalid file path");
        let mut current = self.repo.root.clone();
        let parts: Vec<_> = Path::new(path).components().collect();
        for (index, part) in parts.iter().enumerate() {
            ensure!(matches!(part, Component::Normal(_)), "invalid file path");
            current.push(part.as_os_str());
            match fs::symlink_metadata(&current) {
                Ok(meta) => {
                    ensure!(!meta.file_type().is_symlink(), "refusing a linked file or directory");
                    if index + 1 == parts.len() { ensure!(meta.is_file(), "path is not a regular file"); return Ok(true); }
                    ensure!(meta.is_dir(), "path parent is not a directory");
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(e) => return Err(e.into()),
            }
        }
        bail!("empty file path")
    }

    fn read_working(&self, path: &str) -> Result<Working> {
        ensure!(!self.denied(path), "path is excluded from loop storage");
        if !self.valid_target(path)? { return Ok(Working { bytes: Vec::new(), state: None, mode: None, deleted: true }); }
        let before = self.repo.regular_file(path)?.context("file changed type")?;
        ensure!(before.len() <= self.config.size_limit_bytes, "file exceeds the loop size limit");
        let mut bytes = Vec::new();
        File::open(self.repo.root.join(path))?.take(self.config.size_limit_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
        let after = self.repo.regular_file(path)?.context("file changed type")?;
        ensure!(stamp(&before)? == stamp(&after)? && bytes.len() as u64 == after.len(), "file changed while reading; retry");
        Ok(Working { bytes, state: Some(stamp(&after)?), mode: file_mode(&after), deleted: false })
    }

    pub fn version_bytes(&self, path: &str, version: &Version) -> Result<Vec<u8>> {
        ensure!(version.path == path && !self.denied(path), "version/path mismatch or excluded path");
        if version.deleted { return Ok(Vec::new()); }
        match &version.source {
            Source::Loop(hash) => self.blobs.get(hash),
            Source::Working => Ok(self.read_working(path)?.bytes),
            Source::Absent => Ok(Vec::new()),
            Source::Git(oid) => {
                let size = self.repo.git(&["cat-file", "-s", oid])?;
                let size = std::str::from_utf8(&size.stdout)?.trim().parse::<u64>()?;
                ensure!(size <= self.config.size_limit_bytes, "Git version exceeds the loop size limit");
                let output = self.repo.git(&["cat-file", "blob", oid])?;
                ensure!(output.stdout.len() as u64 == size, "Git object size changed");
                Ok(output.stdout)
            }
        }
    }

    pub fn compare(&self, path: &str, a: &Version, b: &Version) -> Result<String> {
        let old = self.version_bytes(path, a)?;
        let new = self.version_bytes(path, b)?;
        if old.iter().take(8192).any(|byte| *byte == 0) || new.iter().take(8192).any(|byte| *byte == 0) {
            return Ok(if old == new { String::new() } else { "Binary versions differ\n".into() });
        }
        let old_text = String::from_utf8_lossy(&old);
        let new_text = String::from_utf8_lossy(&new);
        let safe_path: String = path.chars().flat_map(|c| if c.is_control() { c.escape_default().collect::<Vec<_>>() } else { vec![c] }).collect();
        Ok(similar::TextDiff::from_lines(old_text.as_ref(), new_text.as_ref()).unified_diff().context_radius(3)
            .header(&format!("a/{safe_path} ({})", a.label), &format!("b/{safe_path} ({})", b.label)).to_string())
    }

    pub fn pin_loop(&self, id: i64, pinned: bool) -> Result<()> {
        ensure!(self.index.execute("UPDATE loops SET pinned=?1 WHERE id=?2 AND worktree=?3", params![pinned, id, self.repo.worktree])? == 1, "loop ID is not in this working tree");
        Ok(())
    }

    fn temporary_pin(&self, id: i64) -> Result<bool> {
        let pinned: bool = self.index.query_row("SELECT pinned FROM loops WHERE id=?1 AND worktree=?2", params![id, self.repo.worktree], |row| row.get(0)).context("target expired since preview; run the preview again")?;
        if pinned { return Ok(false); }
        self.index.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            self.pin_loop(id, true)?;
            self.index.execute("INSERT INTO operation_pins VALUES(?1)", [id])?;
            Ok(())
        })();
        match result {
            Ok(()) => self.index.execute_batch("COMMIT")?,
            Err(error) => { self.index.execute_batch("ROLLBACK")?; return Err(error); }
        }
        Ok(true)
    }

    fn release_temporary_pin(&self, id: i64) -> Result<()> {
        self.index.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            self.index.execute("UPDATE loops SET pinned=0 WHERE id=?1 AND worktree=?2 AND id IN (SELECT id FROM operation_pins)", params![id, self.repo.worktree])?;
            self.index.execute("DELETE FROM operation_pins WHERE id=?1", [id])?;
            Ok(())
        })();
        match result {
            Ok(()) => self.index.execute_batch("COMMIT")?,
            Err(error) => { self.index.execute_batch("ROLLBACK")?; return Err(error); }
        }
        Ok(())
    }

    pub fn preview_at(&mut self, at: i64) -> Result<RewindPreview> {
        self.capture(&[])?;
        let commit = self.commit_before(at)?;
        let mut paths: Vec<_> = self.repo.tracked(&[])?.into_iter().collect();
        paths.sort();
        let mut preview = RewindPreview::default();
        for path in paths {
            let result = (|| -> Result<Option<RewindPlan>> {
                let current = self.read_working(&path)?;
                ensure!(!current.bytes.iter().take(8192).any(|byte| *byte == 0), "binary file is excluded from safety loops");
                let version = self.at_time(&path, at, &commit)?;
                let target = self.version_bytes(&path, &version)?;
                if current.deleted == version.deleted && current.bytes == target { return Ok(None); }
                Ok(Some(RewindPlan { path: path.clone(), version }))
            })();
            match result {
                Ok(Some(plan)) => preview.plans.push(plan),
                Ok(None) => {},
                Err(error) => preview.skipped.push((path, format!("{error:#}"))),
            }
        }
        Ok(preview)
    }

    fn writing_marker(&mut self, path: &str, hash: &str, deleted: bool) -> Result<()> {
        let marker = toml::to_string(&WritingMarker { worktree: self.repo.worktree.clone(), pid: std::process::id(), path: path.into(), expected_hash: hash.into(), deleted, started_ms: now()? })?;
        let file = self.marker_path();
        let previous = fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        let reserve = marker.len() as u64 + 8192;
        if self.bytes()?.saturating_sub(previous).saturating_add(reserve) > self.config.store_cap_bytes {
            let cap = self.config.store_cap_bytes;
            self.config.store_cap_bytes = cap.checked_sub(reserve).context("store cap cannot fit a writing marker")?;
            let result = self.gc();
            self.config.store_cap_bytes = cap;
            result?;
        }
        atomic_write(&file, marker.as_bytes())
    }

    /// All target versions and all safety loops are protected before any write.
    /// Memory stays bounded by one file at a time. A failed batch can be partial;
    /// each completed file has its own safety loop and remains undoable.
    pub fn apply_rewinds(&mut self, plans: &[RewindPlan]) -> Result<Vec<(String, i64)>> {
        let mut unique = HashSet::new();
        for plan in plans { ensure!(unique.insert(&plan.path), "duplicate file in rewind plan"); ensure!(plan.path == plan.version.path, "version/path mismatch"); ensure!(!matches!(plan.version.source, Source::Working), "choose a stored version to rewind to"); }
        let mut protected = Vec::new();
        let mut safety = Vec::new();
        let result = (|| -> Result<Vec<(String, i64)>> {
            for plan in plans {
                if let Some(id) = plan.version.id {
                    let path: String = self.index.query_row("SELECT path FROM loops WHERE id=?1 AND worktree=?2", params![id, self.repo.worktree], |row| row.get(0)).context("target expired since preview; run the preview again")?;
                    ensure!(path == plan.path, "version/path mismatch");
                    if self.temporary_pin(id)? { protected.push(id); }
                }
                let _ = self.version_bytes(&plan.path, &plan.version)?;
                let _ = self.read_working(&plan.path)?;
            }
            for plan in plans {
                let current = self.read_working(&plan.path)?;
                ensure!(!current.bytes.iter().take(8192).any(|byte| *byte == 0), "cannot protect a binary file; rewind refused");
                let hash = blake3::hash(&current.bytes).to_hex().to_string();
                let id = self.save_loop(NewLoop { path: &plan.path, bytes: &current.bytes, hash: &hash, state: current.state, kind: "safety", message: "before rewind", auto_label: false, deleted: current.deleted, pinned: true, mode: current.mode })?;
                protected.push(id);
                safety.push((id, hash, current.deleted));
            }
            crate::fault("after_rewind_safety");
            let mut completed = Vec::new();
            for (plan, (id, previous_hash, previous_deleted)) in plans.iter().zip(&safety) {
                let target = self.version_bytes(&plan.path, &plan.version)?;
                let current = self.read_working(&plan.path)?;
                ensure!(current.deleted == *previous_deleted && blake3::hash(&current.bytes).to_hex().as_str() == previous_hash, "file changed after its safety loop; rewind stopped");
                let hash = blake3::hash(&target).to_hex().to_string();
                self.writing_marker(&plan.path, &hash, plan.version.deleted)?;
                crate::fault("before_rewind_write");
                let destination = self.repo.root.join(&plan.path);
                let existed = self.valid_target(&plan.path)?;
                if plan.version.deleted {
                    if existed {
                        fs::remove_file(&destination)?;
                        #[cfg(unix)]
                        File::open(destination.parent().context("missing parent")?)?.sync_all()?;
                    }
                    self.index.execute("DELETE FROM stat_cache WHERE worktree=?1 AND path=?2", params![self.repo.worktree, plan.path])?;
                } else {
                    atomic_write(&destination, &target)?;
                    #[cfg(unix)]
                    if !existed && let Some(mode) = plan.version.mode {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&destination, fs::Permissions::from_mode(mode))?;
                        File::open(&destination)?.sync_all()?;
                    }
                    crate::fault("after_rewind_write");
                    let written = self.read_working(&plan.path)?;
                    ensure!(!written.deleted && blake3::hash(&written.bytes).to_hex().as_str() == hash, "file changed after rewind; stopped before caching another tool's edit");
                    self.cache_write(&plan.path, written.state.context("rewritten file is absent")?, &hash)?;
                }
                self.remove_writing_marker()?;
                completed.push((plan.path.clone(), *id));
            }
            Ok(completed)
        })();
        for id in protected {
            if let Err(error) = self.release_temporary_pin(id) { self.log(&format!("could not release temporary pin {id}: {error:#}")); }
        }
        self.index.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        if result.is_err() { self.log("rewind stopped; each changed file retains a before-rewind safety loop"); }
        result.context("rewind stopped; each completed file has a safety loop (use rewind <file> undo)")
    }

    pub fn rewind_file(&mut self, input: &Path, selector: &str) -> Result<i64> {
        ensure!(selector != "working", "choose a stored version to rewind to");
        let path = self.repo.relative(input)?;
        // Resolve immutable targets before a fallback scan can evict old data.
        let version = self.resolve(&path, selector)?;
        let was_pinned = if let Some(id) = version.id {
            Some((id, self.temporary_pin(id)?))
        } else { None };
        let result = (|| {
            self.capture(&[])?;
            self.apply_rewinds(&[RewindPlan { path, version }])?.first().map(|(_, id)| *id).context("no file rewound")
        })();
        if let Some((id, true)) = was_pinned { let _ = self.release_temporary_pin(id); }
        result
    }
}
