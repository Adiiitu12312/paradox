use anyhow::{Context, Result, ensure};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::blob::{BlobStore, LocalStore, atomic_write, directory, reject_link};
use crate::config::Config;
use crate::repo::Repo;

pub struct LoopStore {
    pub repo: Repo,
    pub config: Config,
    pub blobs: LocalStore,
    pub index: Connection,
    denylist: GlobSet,
    pending_stats: Vec<(String, (i64, u64))>,
    checked_dirs: HashSet<std::path::PathBuf>,
    _lock: File,
}

#[derive(Debug, Default)]
pub struct CaptureReport {
    pub saved: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub hashed: usize,
}

#[derive(Clone, Debug)]
pub struct Loop {
    pub id: i64,
    pub path: String,
    pub blob_hash: String,
    pub ts: i64,
    pub message: String,
    pub auto_label: bool,
    pub head_sha: String,
    pub pinned: bool,
    pub size: u64,
    pub kind: String,
    pub deleted: bool,
    pub mode: Option<u32>,
}

pub(crate) struct NewLoop<'a> {
    pub path: &'a str,
    pub bytes: &'a [u8],
    pub hash: &'a str,
    pub state: Option<(i64, u64)>,
    pub kind: &'a str,
    pub message: &'a str,
    pub auto_label: bool,
    pub deleted: bool,
    pub pinned: bool,
    pub mode: Option<u32>,
}

pub(crate) fn stamp(meta: &fs::Metadata) -> Result<(i64, u64)> {
    let time = meta.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
    Ok((
        i64::try_from(time).context("file time exceeds supported range")?,
        meta.len(),
    ))
}

pub(crate) fn file_mode(meta: &fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

pub fn now() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

fn sql_size(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(column)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

impl LoopStore {
    /// Every mutating operation holds this advisory lock for its whole lifetime.
    pub fn open(repo: Repo) -> Result<Self> {
        // Install the exclusion before creating any sidecar files. If exclude
        // cannot be updated, fail without exposing store contents to git add.
        Self::exclude_metadata(&repo)?;
        directory(&repo.store)?;
        let lock_path = repo.store.join("lock");
        reject_link(&lock_path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        fs2::FileExt::lock_exclusive(&lock)?;
        let config = Config::load(&repo.store)?;
        for file in ["index.sqlite3", "index.sqlite3-wal", "index.sqlite3-shm"] {
            reject_link(&repo.store.join(file))?;
        }
        let index_path = repo.store.join("index.sqlite3");
        let existing = fs::metadata(&index_path).is_ok_and(|m| m.len() > 0);
        let index = Connection::open(index_path)?;
        index.busy_timeout(Duration::from_secs(5))?;
        let application: i64 = index.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        ensure!(
            !existing || application == 0x50584431,
            "index is not a recognized Paradox store"
        );
        if !existing {
            index.execute_batch("PRAGMA application_id=1347961905;")?;
        }
        index.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS schema_version(version INTEGER NOT NULL);",
        )?;
        let version: Option<i64> = index
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .optional()?;
        match version {
            None => index.execute_batch("BEGIN IMMEDIATE;
                CREATE TABLE loops(id INTEGER PRIMARY KEY AUTOINCREMENT,worktree TEXT NOT NULL,path TEXT NOT NULL,
                    blob_hash TEXT NOT NULL,ts INTEGER NOT NULL,message TEXT NOT NULL,auto_label INTEGER NOT NULL,
                    head_sha TEXT NOT NULL,pinned INTEGER NOT NULL DEFAULT 0,size INTEGER NOT NULL,kind TEXT NOT NULL
                    CHECK(kind IN ('save','scan','safety')),deleted INTEGER NOT NULL DEFAULT 0,mode INTEGER);
                CREATE INDEX loops_path_ts ON loops(worktree,path,ts);
                CREATE INDEX loops_head ON loops(head_sha);
                CREATE INDEX loops_blob ON loops(blob_hash);
                CREATE TABLE operation_pins(id INTEGER PRIMARY KEY REFERENCES loops(id) ON DELETE CASCADE);
                CREATE TABLE stat_cache(worktree TEXT NOT NULL,path TEXT NOT NULL,mtime_ns INTEGER NOT NULL,
                    size INTEGER NOT NULL,last_hash TEXT NOT NULL,PRIMARY KEY(worktree,path));
                CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
                CREATE TABLE blobs(hash TEXT PRIMARY KEY,stored_size INTEGER NOT NULL);
                CREATE TABLE tracked_cache(worktree TEXT NOT NULL,path TEXT NOT NULL,fingerprint TEXT NOT NULL,PRIMARY KEY(worktree,path));
                INSERT INTO meta VALUES('blob_bytes','0');
                INSERT INTO schema_version VALUES(2); COMMIT;")?,
            Some(1) => index.execute_batch("BEGIN IMMEDIATE; ALTER TABLE loops ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0; ALTER TABLE loops ADD COLUMN mode INTEGER; CREATE TABLE operation_pins(id INTEGER PRIMARY KEY REFERENCES loops(id) ON DELETE CASCADE); UPDATE schema_version SET version=2; COMMIT;")?,
            Some(2) => {}
            Some(v) => anyhow::bail!("unsupported loop store schema version {v}"),
        }
        // The exclusive repository lock proves any remaining operation leases
        // came from a terminated process. Permanent user pins are not in here.
        if index.query_row("SELECT EXISTS(SELECT 1 FROM operation_pins)", [], |row| {
            row.get::<_, bool>(0)
        })? {
            index.execute_batch("BEGIN IMMEDIATE; UPDATE loops SET pinned=0 WHERE id IN (SELECT id FROM operation_pins); DELETE FROM operation_pins; COMMIT;")?;
        }
        let mut builder = GlobSetBuilder::new();
        // Baseline secret protection cannot be disabled by replacing the config list.
        for pattern in Config::default()
            .secrets_denylist
            .iter()
            .chain(&config.secrets_denylist)
        {
            builder.add(GlobBuilder::new(pattern).case_insensitive(true).build()?);
        }
        let denylist = builder.build()?;
        let blobs = LocalStore::new(repo.store.join("blobs"), config.size_limit_bytes)?;
        let store = Self {
            repo,
            config,
            blobs,
            index,
            denylist,
            pending_stats: Vec::new(),
            checked_dirs: HashSet::new(),
            _lock: lock,
        };
        if let Err(error) = store.recover_writing_marker() {
            store.log(&format!("writing marker recovery: {error:#}"));
        }
        Ok(store)
    }

    fn exclude_metadata(repo: &Repo) -> Result<()> {
        let info = repo.common.join("info");
        directory(&info)?;
        let path = info.join("exclude");
        reject_link(&path)?;
        let mut bytes = if path.exists() {
            fs::read(&path)?
        } else {
            Vec::new()
        };
        if !bytes.split(|b| *b == b'\n').any(|s| s == b"/.paradox/") {
            if !bytes.is_empty() && !bytes.ends_with(b"\n") {
                bytes.push(b'\n');
            }
            bytes.extend_from_slice(b"/.paradox/\n");
            atomic_write(&path, &bytes)?;
        }
        Ok(())
    }

    pub(crate) fn denied(&self, path: &str) -> bool {
        path.split('/')
            .any(|part| matches!(part, ".git" | ".paradox") || self.denylist.is_match(part))
            || self.denylist.is_match(path)
    }

    pub fn loops(&self, path: &str) -> Result<Vec<Loop>> {
        let mut statement = self.index.prepare("SELECT id,path,blob_hash,ts,message,auto_label,head_sha,pinned,size,kind,deleted,mode FROM loops WHERE worktree=?1 AND path=?2 ORDER BY ts DESC,id DESC")?;
        Ok(statement
            .query_map(params![self.repo.worktree, path], |r| {
                Ok(Loop {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    blob_hash: r.get(2)?,
                    ts: r.get(3)?,
                    message: r.get(4)?,
                    auto_label: r.get(5)?,
                    head_sha: r.get(6)?,
                    pinned: r.get(7)?,
                    size: sql_size(r, 8)?,
                    kind: r.get(9)?,
                    deleted: r.get(10)?,
                    mode: r.get(11)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn cache(&self, path: &str) -> Result<Option<(i64, u64, String)>> {
        Ok(self
            .index
            .query_row(
                "SELECT mtime_ns,size,last_hash FROM stat_cache WHERE worktree=?1 AND path=?2",
                params![self.repo.worktree, path],
                |r| Ok((r.get(0)?, sql_size(r, 1)?, r.get(2)?)),
            )
            .optional()?)
    }

    pub(crate) fn cache_write(&self, path: &str, state: (i64, u64), hash: &str) -> Result<()> {
        self.index.execute("INSERT INTO stat_cache VALUES(?1,?2,?3,?4,?5) ON CONFLICT(worktree,path) DO UPDATE SET mtime_ns=excluded.mtime_ns,size=excluded.size,last_hash=excluded.last_hash",
            params![self.repo.worktree,path,state.0,i64::try_from(state.1)?,hash])?;
        Ok(())
    }

    /// Explicit files are checked against Git using literal pathspecs; no tree scan.
    /// An empty selection scans only Git's tracked set, never new directories.
    pub fn capture(&mut self, inputs: &[std::path::PathBuf]) -> Result<CaptureReport> {
        self.capture_inner(inputs, None)
    }

    /// Watch uses the ordinary locked capture path, including dedup and skips.
    pub fn capture_watch(&mut self, inputs: &[std::path::PathBuf], bulk: bool) -> Result<CaptureReport> {
        self.capture_inner(inputs, bulk.then_some("bulk change"))
    }

    fn capture_inner(&mut self, inputs: &[std::path::PathBuf], message: Option<&str>) -> Result<CaptureReport> {
        let started = std::time::Instant::now();
        let profile = std::env::var_os("PARADOX_PROFILE_CAPTURE").is_some();
        self.checked_dirs.clear();
        let selected = inputs
            .iter()
            .map(|p| self.repo.relative(p))
            .collect::<Result<Vec<_>>>()?;
        let signature = self.repo.index_signature();
        let cached = !selected.is_empty()
            && signature.as_ref().is_some_and(|signature| {
                selected.iter().all(|path| {
                    self.index
                    .query_row(
                        "SELECT fingerprint=?3 FROM tracked_cache WHERE worktree=?1 AND path=?2",
                        params![self.repo.worktree, path, signature],
                        |r| r.get::<_, bool>(0),
                    )
                    .optional()
                    .ok()
                    .flatten()
                    == Some(true)
                })
            });
        let mut tracked: Vec<_> = if cached {
            selected.clone()
        } else {
            self.repo.tracked(&selected)?.into_iter().collect()
        };
        if !cached
            && !selected.is_empty()
            && let Some(signature) = signature
        {
            for path in &tracked {
                self.index.execute("INSERT INTO tracked_cache VALUES(?1,?2,?3) ON CONFLICT(worktree,path) DO UPDATE SET fingerprint=excluded.fingerprint",params![self.repo.worktree,path,signature])?;
            }
        }
        tracked.sort();
        if profile {
            eprintln!("capture profile: tracked {:?}", started.elapsed());
        }
        let all = inputs.is_empty();
        if !all && !cached {
            self.refresh_staged_renames()?;
        }
        // Only a stat-changed file without a prior captured hash needs Git's
        // dirty set to distinguish its HEAD baseline. Warm scans avoid a second
        // full filesystem walk by Git.
        let mut dirty = if all {
            self.refresh_git_changes()?
        } else {
            None
        };
        let cache_map: Option<HashMap<String, (i64, u64, String)>> = if all {
            Some(
                self.index
                    .prepare(
                        "SELECT path,mtime_ns,size,last_hash FROM stat_cache WHERE worktree=?1",
                    )?
                    .query_map([&self.repo.worktree], |r| {
                        Ok((r.get(0)?, (r.get(1)?, sql_size(r, 2)?, r.get(3)?)))
                    })?
                    .collect::<rusqlite::Result<_>>()?,
            )
        } else {
            None
        };
        let metadata = if all {
            self.repo.scan_metadata(&tracked)?
        } else {
            tracked
                .iter()
                .map(|path| self.repo.regular_file_cached(path, &mut self.checked_dirs))
                .collect()
        };
        if profile {
            eprintln!("capture profile: metadata {:?}", started.elapsed());
        }
        let mut report = CaptureReport::default();
        for (path, meta) in tracked.into_iter().zip(metadata) {
            let cache = match &cache_map {
                Some(map) => map.get(&path).cloned(),
                None => self.cache(&path)?,
            };
            match self.capture_one(
                &path,
                (if all || message.is_some() { "scan" } else { "save" }, message),
                all,
                &mut dirty,
                cache,
                meta,
            ) {
                Ok((saved, hashed, skipped)) => {
                    report.saved += usize::from(saved);
                    report.hashed += usize::from(hashed);
                    report.skipped += usize::from(skipped);
                    report.unchanged += usize::from(!saved && !skipped);
                }
                Err(error) => {
                    self.log(&format!("capture {path}: {error:#}"));
                    report.skipped += 1;
                }
            }
        }
        if !self.pending_stats.is_empty() {
            self.index.execute_batch("BEGIN IMMEDIATE")?;
            let pending = std::mem::take(&mut self.pending_stats);
            for (path, state) in pending {
                if let Err(error) = self.cache_write(&path, state, "") {
                    self.index.execute_batch("ROLLBACK")?;
                    return Err(error);
                }
            }
            self.index.execute_batch("COMMIT")?;
        }
        if profile {
            eprintln!("capture profile: files {:?}", started.elapsed());
        }
        // Bound the WAL after each short-lived command. Blob bytes are durable
        // before references are committed; orphan cleanup can always run later.
        self.index
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        if self.bytes()? > self.config.store_cap_bytes {
            self.gc()?;
        }
        if profile {
            eprintln!("capture profile: durable {:?}", started.elapsed());
        }
        Ok(report)
    }

    fn capture_one(
        &mut self,
        path: &str,
        kind: (&str, Option<&str>),
        all: bool,
        dirty: &mut Option<HashSet<String>>,
        cache: Option<(i64, u64, String)>,
        meta: Result<Option<fs::Metadata>>,
    ) -> Result<(bool, bool, bool)> {
        if self.denied(path) {
            return Ok((false, false, true));
        }
        let Some(meta) = meta? else {
            return Ok((false, false, true));
        };
        if meta.len() > self.config.size_limit_bytes {
            return Ok((false, false, true));
        }
        let state = stamp(&meta)?;
        if cache.as_ref().is_some_and(|c| (c.0, c.1) == state) {
            return Ok((false, false, false));
        }
        if all && cache.as_ref().is_none_or(|c| c.2.is_empty()) {
            if dirty.is_none() {
                *dirty = Some(self.repo.dirty()?);
            }
            if dirty.as_ref().is_some_and(|set| !set.contains(path)) {
                self.pending_stats.push((path.into(), state));
                return Ok((false, false, false));
            }
        }
        let mut bytes = Vec::new();
        File::open(self.repo.root.join(path))?
            .take(self.config.size_limit_bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > self.config.size_limit_bytes
            || bytes.iter().take(8192).any(|b| *b == 0)
        {
            return Ok((false, false, true));
        }
        let after = self
            .repo
            .regular_file(path)?
            .context("file changed type during capture")?;
        ensure!(
            stamp(&after)? == state && bytes.len() as u64 == state.1,
            "file changed during capture; next save will retry"
        );
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let latest = self.index.query_row("SELECT blob_hash,deleted FROM loops WHERE worktree=?1 AND path=?2 ORDER BY ts DESC,id DESC LIMIT 1",
            params![self.repo.worktree,path], |r| Ok((r.get::<_,String>(0)?, r.get::<_,bool>(1)?))).optional()?;
        if latest
            .as_ref()
            .is_some_and(|(old, deleted)| !deleted && old == &hash)
        {
            self.cache_write(path, state, &hash)?;
            return Ok((false, true, false));
        }
        let previous = match latest {
            Some((old, false)) => self.blobs.get(&old)?,
            _ => Vec::new(),
        };
        let (message, auto_label) = match kind.1 {
            Some(message) => (message.to_string(), true),
            None => crate::message::label(Path::new(path), &bytes, &previous),
        };
        self.save_loop(NewLoop {
            path,
            bytes: &bytes,
            hash: &hash,
            state: Some(state),
            kind: kind.0,
            message: &message,
            auto_label,
            deleted: false,
            pinned: false,
            mode: file_mode(&after),
        })?;
        Ok((true, true, false))
    }

    /// The only blob/reference insertion path, shared by saves and safety loops.
    pub(crate) fn save_loop(&mut self, input: NewLoop<'_>) -> Result<i64> {
        let NewLoop {
            path,
            bytes,
            hash,
            state,
            kind,
            message,
            auto_label,
            deleted,
            pinned,
            mode,
        } = input;
        let stored_size = i64::try_from(self.blobs.put(hash, bytes)?)?;
        crate::fault("after_blob");
        self.index.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<(i64, Vec<String>)> {
            if self.index.execute(
                "INSERT OR IGNORE INTO blobs VALUES(?1,?2)",
                params![hash, stored_size],
            )? > 0
            {
                self.index.execute(
                    "UPDATE meta SET value=CAST(value AS INTEGER)+?1 WHERE key='blob_bytes'",
                    [stored_size],
                )?;
            }
            self.index.execute("INSERT INTO loops(worktree,path,blob_hash,ts,message,auto_label,head_sha,pinned,size,kind,deleted,mode) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![self.repo.worktree,path,hash,now()?,message,auto_label,self.repo.head,pinned,i64::try_from(bytes.len())?,kind,deleted,mode])?;
            let id = self.index.last_insert_rowid();
            if pinned {
                self.index
                    .execute("INSERT INTO operation_pins VALUES(?1)", [id])?;
            }
            crate::fault("after_index_insert");
            if let Some(state) = state {
                self.cache_write(path, state, hash)?;
            } else {
                self.index.execute(
                    "DELETE FROM stat_cache WHERE worktree=?1 AND path=?2",
                    params![self.repo.worktree, path],
                )?;
            }
            let deleted = self.evict()?;
            ensure!(
                self.index.query_row(
                    "SELECT EXISTS(SELECT 1 FROM loops WHERE id=?1)",
                    [id],
                    |r| r.get::<_, bool>(0)
                )?,
                "store cap cannot retain this loop; pinned loops were preserved"
            );
            Ok((id, deleted))
        })();
        match result {
            Ok((id, deleted)) => {
                let compact = !deleted.is_empty();
                self.index.execute_batch("COMMIT")?;
                crate::fault("after_gc_commit");
                for hash in deleted {
                    self.blobs.delete(&hash)?;
                }
                if compact {
                    self.index
                        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
                }
                Ok(id)
            }
            Err(error) => {
                self.index.execute_batch("ROLLBACK")?;
                // This reference was never committed. Remove only unreferenced data.
                if !self.index.query_row(
                    "SELECT EXISTS(SELECT 1 FROM blobs WHERE hash=?1)",
                    [hash],
                    |r| r.get::<_, bool>(0),
                )? {
                    self.blobs.delete(hash)?;
                }
                Err(error)
            }
        }
    }

    fn metadata_bytes(&self) -> Result<u64> {
        // WAL is short-lived and truncated on successful command completion.
        let pages: u64 = self
            .index
            .query_row("PRAGMA page_count", [], |r| sql_size(r, 0))?;
        let free: u64 = self
            .index
            .query_row("PRAGMA freelist_count", [], |r| sql_size(r, 0))?;
        let page_size: u64 = self
            .index
            .query_row("PRAGMA page_size", [], |r| sql_size(r, 0))?;
        let other: u64 = [
            "index.sqlite3-shm",
            "lock",
            "log",
            "config.toml",
            "tracked.toml",
        ]
        .iter()
        .filter_map(|p| fs::metadata(self.repo.store.join(p)).ok())
        .map(|m| m.len())
        .sum();
        let mut markers = 0;
        for entry in fs::read_dir(&self.repo.store)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if ((name.starts_with("writing-") && name.ends_with(".toml"))
                || (name.starts_with("setup-") && name.ends_with(".json"))
                || matches!(name.as_ref(), "watch.json" | "watch.stop"))
                && entry.file_type()?.is_file()
            {
                markers += entry.metadata()?.len().max(4096);
            }
        }
        Ok(pages.saturating_sub(free) * page_size + other + markers + 8192)
    }

    pub fn bytes(&self) -> Result<u64> {
        let blobs: u64 = self.index.query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key='blob_bytes'",
            [],
            |r| sql_size(r, 0),
        )?;
        Ok(blobs + self.metadata_bytes()?)
    }

    fn evict(&self) -> Result<Vec<String>> {
        let mut garbage = Vec::new();
        if self.bytes()? <= self.config.store_cap_bytes {
            return Ok(garbage);
        }
        let target = (self.config.store_cap_bytes as u128 * 9 / 10) as u64;
        while self.bytes()? > target {
            let oldest = self
                .index
                .query_row(
                    "SELECT id,blob_hash FROM loops WHERE pinned=0 ORDER BY ts,id LIMIT 1",
                    [],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()?;
            let Some((id, hash)) = oldest else {
                anyhow::bail!("store cap is occupied by pinned loops or metadata");
            };
            self.index.execute("DELETE FROM loops WHERE id=?1", [id])?;
            if !self.index.query_row(
                "SELECT EXISTS(SELECT 1 FROM loops WHERE blob_hash=?1)",
                [&hash],
                |r| r.get::<_, bool>(0),
            )? {
                let removed: i64 = self.index.query_row(
                    "SELECT stored_size FROM blobs WHERE hash=?1",
                    [&hash],
                    |r| r.get(0),
                )?;
                self.index
                    .execute("DELETE FROM blobs WHERE hash=?1", [&hash])?;
                self.index.execute(
                    "UPDATE meta SET value=CAST(value AS INTEGER)-?1 WHERE key='blob_bytes'",
                    [removed],
                )?;
                garbage.push(hash);
            }
            crate::fault("during_gc");
        }
        Ok(garbage)
    }

    /// Commit reference deletion before removing files; crashes leave only orphans.
    pub fn gc(&mut self) -> Result<()> {
        // These are disposable accelerators, not history. Reclaim them before
        // evicting content when metadata itself is putting pressure on the cap.
        if self.bytes()? > self.config.store_cap_bytes
            && !self
                .index
                .query_row("SELECT EXISTS(SELECT 1 FROM operation_pins)", [], |row| {
                    row.get::<_, bool>(0)
                })?
        {
            self.index.execute_batch("DELETE FROM stat_cache; DELETE FROM tracked_cache; PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        }
        self.index.execute_batch("BEGIN IMMEDIATE")?;
        let garbage = match self.evict() {
            Ok(g) => g,
            Err(e) => {
                self.index.execute_batch("ROLLBACK")?;
                return Err(e);
            }
        };
        self.index.execute_batch("COMMIT")?;
        crate::fault("after_gc_commit");
        for hash in garbage {
            self.blobs.delete(&hash)?;
        }
        let referenced: HashSet<String> = self
            .index
            .prepare("SELECT hash FROM blobs")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for prefix in fs::read_dir(&self.blobs.root)? {
            let prefix = prefix?;
            if !prefix.file_type()?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(prefix.path())? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let hash = format!("{}{}", prefix.file_name().to_string_lossy(), name);
                if name.starts_with(".pending-")
                    || (hash.len() == 64 && !referenced.contains(&hash))
                {
                    fs::remove_file(entry.path())?;
                }
            }
        }
        for entry in fs::read_dir(&self.repo.store)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry.file_name().to_string_lossy().starts_with(".pending-")
            {
                fs::remove_file(entry.path())?;
            }
        }
        self.index.execute_batch(
            "PRAGMA wal_checkpoint(TRUNCATE); VACUUM; PRAGMA wal_checkpoint(TRUNCATE);",
        )?;
        Ok(())
    }

    pub fn log(&self, message: &str) {
        log_at(&self.repo.store, message);
    }
}

pub fn log_at(root: &Path, message: &str) {
    // Store opening may have failed because this directory is a link. Logging
    // that failure must not follow it and write outside the repository.
    if !fs::symlink_metadata(root).is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink())
    {
        return;
    }
    let path = root.join("log");
    if reject_link(&path).is_err() {
        return;
    }
    if fs::metadata(&path).is_ok_and(|m| m.len() > 4096) {
        let _ = fs::write(&path, b"");
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let message: String = message.chars().take(1024).collect();
        let _ = writeln!(file, "{} {message}", now().unwrap_or_default());
    }
}
