//! Explicit, session-only native filesystem watching. No polling or service setup.
use crate::blob::{atomic_write, reject_link};
use crate::repo::Repo;
use crate::store::{LoopStore, log_at, now};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

#[derive(Clone, Serialize, Deserialize)]
pub struct Status {
    pub pid: u32,
    pub session: String,
    pub started_ms: i64,
    pub loops_created: u64,
    pub tracked_files: usize,
    pub idle_minutes: u64,
}

enum Message {
    Event(notify::Result<Event>, Instant),
    Stop,
}

struct SessionFiles(Repo);
impl Drop for SessionFiles {
    fn drop(&mut self) {
        for path in [state_path(&self.0), stop_path(&self.0)] {
            if let Err(error) = fs::remove_file(path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                log_at(&self.0.store, &format!("watch cleanup: {error}"));
            }
        }
    }
}

fn lock_path(repo: &Repo) -> PathBuf {
    repo.store.join("watch.lock")
}
fn state_path(repo: &Repo) -> PathBuf {
    repo.store.join("watch.json")
}
fn stop_path(repo: &Repo) -> PathBuf {
    repo.store.join("watch.stop")
}

fn open_lock(repo: &Repo) -> Result<File> {
    let path = lock_path(repo);
    reject_link(&path)?;
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?)
}

fn read_status(repo: &Repo) -> Result<Status> {
    let path = state_path(repo);
    reject_link(&path)?;
    let meta = fs::metadata(&path)?;
    ensure!(meta.len() <= 8192, "watch status exceeds its limit");
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

/// A held kernel lock establishes liveness; stale PID contents do not.
pub fn status(repo: &Repo) -> Result<Option<Status>> {
    if !lock_path(repo).exists() {
        return Ok(None);
    }
    let file = open_lock(repo)?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            FileExt::unlock(&file)?;
            Ok(None)
        }
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            Ok(Some(read_status(repo)?))
        }
        Err(error) => Err(error.into()),
    }
}

pub fn stop(repo: &Repo) -> Result<bool> {
    let Some(state) = status(repo)? else {
        return Ok(false);
    };
    atomic_write(&stop_path(repo), state.session.as_bytes())?;
    Ok(true)
}

pub fn session_token() -> Result<String> {
    let temp = tempfile::NamedTempFile::new()?;
    Ok(blake3::hash(
        format!(
            "{}:{}:{}",
            std::process::id(),
            now()?,
            temp.path().display()
        )
        .as_bytes(),
    )
    .to_hex()
    .to_string())
}

fn normalized(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn excluded(path: &str) -> bool {
    path.split('/').any(|part| {
        matches!(
            part,
            ".git" | ".paradox" | "node_modules" | "target" | "dist" | "build" | "out" | ".next"
        )
    })
}

#[cfg(target_os = "linux")]
fn inotify_limit() -> Option<usize> {
    #[cfg(feature = "fault-injection")]
    if let Ok(value) = std::env::var("PARADOX_TEST_INOTIFY_LIMIT") {
        return value.parse().ok();
    }
    fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn safe_directory(root: &Path, relative: &Path) -> Option<PathBuf> {
    let mut directory = root.to_path_buf();
    let metadata = fs::symlink_metadata(&directory).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return None;
        };
        directory.push(name);
        // Check each unique parent independently of file existence: a
        // missing final file must stay covered for recreation, while a
        // replaced directory must never redirect the native watch.
        let metadata = fs::symlink_metadata(&directory).ok()?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return None;
        }
    }
    let directory = fs::canonicalize(directory).ok()?;
    directory.starts_with(root).then_some(directory)
}

fn tracked(repo: &Repo) -> Result<TrackedPaths> {
    let output = repo.git(&["--literal-pathspecs", "ls-files", "--cached", "-z", "--"])?;
    let extra = repo.extra_tracked()?;
    // Borrow Git's NUL records instead of allocating a String/hash bucket for
    // every path, only to free those allocations before the idle session.
    let mut paths: Vec<_> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|bytes| !bytes.is_empty())
        .filter_map(|bytes| std::str::from_utf8(bytes).ok())
        .chain(extra.iter().map(String::as_str))
        .collect();
    let mut checked_dirs = HashSet::new();
    paths.retain(|path| {
        !excluded(path) && !repo.root.join(path).starts_with(&repo.common) && repo.regular_file_cached(path, &mut checked_dirs).is_ok()
    });
    // Git rejects check-ignore pathspecs that traverse a symlink. Validate
    // unique parents before ignore IPC, preserving missing final files while
    // excluding paths whose directories would redirect native coverage.
    let root = normalized(&repo.root);
    let parents: BTreeSet<_> = paths
        .iter()
        .copied()
        .filter_map(|name| Path::new(name).parent())
        .collect();
    let parents: BTreeSet<_> = parents
        .into_iter()
        .filter(|parent| safe_directory(&root, parent).is_some())
        .collect();
    paths.retain(|name| {
        Path::new(*name)
            .parent()
            .is_some_and(|parent| parents.contains(parent))
    });
    let ignored = ignored_paths(repo, &paths)?;
    let mut ignored: Vec<_> = ignored
        .split(|byte| *byte == 0)
        .filter_map(|bytes| std::str::from_utf8(bytes).ok())
        .collect();
    ignored.sort_unstable();
    paths.retain(|path| ignored.binary_search(path).is_err());
    TrackedPaths::from_paths(paths)
}

fn ignored_paths(repo: &Repo, paths: &[&str]) -> Result<Vec<u8>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = repo
        .command(&["check-ignore", "--no-index", "--stdin", "-z"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child.stdin.take().context("missing Git ignore input")?;
    // Read stdout concurrently with writing stdin: a large ignored tracked set
    // must never deadlock when both OS pipes fill.
    let output = std::thread::scope(|scope| -> Result<_> {
        let writer = scope.spawn(move || -> std::io::Result<()> {
            for path in paths {
                input.write_all(path.as_bytes())?;
                input.write_all(&[0])?;
            }
            Ok(())
        });
        let output = child.wait_with_output()?;
        writer
            .join()
            .map_err(|_| anyhow::anyhow!("Git ignore input writer failed"))??;
        Ok(output)
    })?;
    ensure!(
        matches!(output.status.code(), Some(0 | 1)),
        "Git ignore check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

fn remove_ignored(repo: &Repo, paths: &mut HashSet<String>) -> Result<()> {
    let names: Vec<_> = paths.iter().map(String::as_str).collect();
    let ignored = ignored_paths(repo, &names)?;
    drop(names);
    for path in ignored
        .split(|byte| *byte == 0)
        .filter_map(|bytes| std::str::from_utf8(bytes).ok())
    {
        paths.remove(path);
    }
    Ok(())
}

/// Exact sorted paths without one heap allocation and hash bucket per file.
#[derive(Default)]
struct TrackedPaths {
    arena: Box<str>,
    starts: Box<[usize]>,
}

impl TrackedPaths {
    fn from_paths(mut paths: Vec<&str>) -> Result<Self> {
        paths.sort_unstable();
        paths.dedup();
        let capacity = paths.iter().map(|path| path.len() + 1).sum();
        let mut arena = String::with_capacity(capacity);
        let mut starts = Vec::with_capacity(paths.len());
        for path in paths {
            ensure!(
                !path.is_empty() && !path.contains('\0'),
                "invalid tracked path"
            );
            starts.push(arena.len());
            arena.push_str(path);
            arena.push('\0');
        }
        Ok(Self {
            arena: arena.into_boxed_str(),
            starts: starts.into_boxed_slice(),
        })
    }

    fn len(&self) -> usize {
        self.starts.len()
    }

    fn path_at(&self, index: usize) -> &str {
        let start = self.starts[index];
        let end = self
            .starts
            .get(index + 1)
            .copied()
            .unwrap_or(self.arena.len())
            - 1;
        &self.arena[start..end]
    }

    fn contains(&self, path: &str) -> bool {
        let mut left = 0;
        let mut right = self.len();
        while left < right {
            let middle = left + (right - left) / 2;
            match self.path_at(middle).cmp(path) {
                std::cmp::Ordering::Less => left = middle + 1,
                std::cmp::Ordering::Greater => right = middle,
                std::cmp::Ordering::Equal => return true,
            }
        }
        false
    }

    fn iter(&self) -> impl Iterator<Item = &str> {
        self.arena.split_terminator('\0')
    }

    fn difference<'a>(&'a self, other: &'a Self) -> impl Iterator<Item = &'a str> + 'a {
        self.iter().filter(move |path| !other.contains(path))
    }
}

struct Coverage {
    files: TrackedPaths,
    watched: BTreeSet<PathBuf>,
    index: PathBuf,
    root: PathBuf,
}

impl Coverage {
    fn new(repo: &Repo) -> Result<Self> {
        let index = repo.git(&["rev-parse", "--path-format=absolute", "--git-path", "index"])?;
        Ok(Self {
            files: TrackedPaths::default(),
            watched: BTreeSet::new(),
            index: PathBuf::from(
                std::str::from_utf8(&index.stdout)?.trim_end_matches(['\r', '\n']),
            ),
            root: normalized(&repo.root),
        })
    }

    fn directory(&self, relative: &Path) -> Option<PathBuf> {
        safe_directory(&self.root, relative)
    }

    fn refresh(&mut self, repo: &Repo, watcher: &mut RecommendedWatcher) -> Result<()> {
        let files = tracked(repo)?;
        let controls: BTreeSet<_> = [
            self.index
                .parent()
                .context("index has no directory")?
                .to_path_buf(),
            repo.store.clone(),
        ]
        .into_iter()
        .map(|p| normalized(&p))
        .collect();
        let parents: BTreeSet<_> = files
            .iter()
            .filter_map(|name| Path::new(name).parent())
            .collect();
        let mut directories: BTreeSet<_> = parents
            .into_iter()
            .filter_map(|parent| self.directory(parent))
            .collect();
        #[cfg(target_os = "linux")]
        {
            if let Some(limit) = inotify_limit() {
                let available = limit.saturating_sub(controls.len());
                if directories.len() > available {
                    let omitted: Vec<_> = directories
                        .iter()
                        .skip(available)
                        .map(|p| p.display().to_string())
                        .collect();
                    directories = directories.into_iter().take(available).collect();
                    eprintln!(
                        "Paradox watch: inotify limit {limit}; these directories are NOT covered: {}. Raise fs.inotify.max_user_watches with sysctl (for example sudo sysctl fs.inotify.max_user_watches=524288).",
                        omitted.join(", ")
                    );
                }
            }
        }
        directories.extend(controls);
        for path in self.watched.difference(&directories) {
            if let Err(error) = watcher.unwatch(path) {
                log_at(
                    &repo.store,
                    &format!("watch unwatch {}: {error}", path.display()),
                );
            }
        }
        let mut active: BTreeSet<_> = self.watched.intersection(&directories).cloned().collect();
        for path in directories.difference(&self.watched) {
            match watcher.watch(path, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    active.insert(path.clone());
                }
                Err(error) => {
                    let warning = format!(
                        "watch coverage missing {}: {error}; on Linux raise fs.inotify.max_user_watches",
                        path.display()
                    );
                    eprintln!("Paradox {warning}");
                    log_at(&repo.store, &warning);
                }
            }
        }
        self.files = files;
        self.watched = active;
        Ok(())
    }

    fn relative(&self, path: &Path) -> Option<String> {
        // Resolve directory aliases, never a final symlink saved by an editor.
        let parent = normalized(path.parent()?);
        let absolute = parent.join(path.file_name()?);
        let relative = absolute.strip_prefix(&self.root).ok()?;
        #[cfg(windows)]
        let name = relative.to_str()?.replace('\\', "/");
        #[cfg(not(windows))]
        let name = relative.to_str()?.to_string();
        self.files.contains(&name).then_some(name)
    }
}

fn idle_duration(minutes: u64) -> Option<Duration> {
    #[cfg(feature = "fault-injection")]
    if let Ok(value) = std::env::var("PARADOX_TEST_WATCH_IDLE_MS")
        && let Ok(value) = value.parse::<u64>()
    {
        return Some(Duration::from_millis(value));
    }
    (minutes != 0).then(|| Duration::from_secs(minutes.saturating_mul(60)))
}

fn persist_status(repo: &Repo, state: &Status) -> Result<()> {
    // Watch controls are bounded; include their budget alongside existing data.
    let store = LoopStore::open(repo.clone())?;
    let bytes = serde_json::to_vec(state)?;
    let previous = fs::metadata(state_path(repo))
        .map(|m| m.len().max(4096))
        .unwrap_or(0);
    ensure!(
        store
            .bytes()?
            .saturating_sub(previous)
            .saturating_add((bytes.len() as u64).max(4096))
            <= store.config.store_cap_bytes,
        "watch status does not fit the store cap"
    );
    atomic_write(&state_path(repo), &bytes)
}

fn receive(receiver: &Receiver<Message>, timeout: Option<Duration>) -> Result<Option<Message>> {
    if let Some(timeout) = timeout {
        match receiver.recv_timeout(timeout) {
            Ok(message) => Ok(Some(message)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => anyhow::bail!("native watcher disconnected"),
        }
    } else {
        Ok(Some(receiver.recv()?))
    }
}

pub fn run(repo: Repo, token: String) -> Result<()> {
    let config = {
        let store = LoopStore::open(repo.clone())?;
        store.config.clone()
    };
    let lock = open_lock(&repo)?;
    lock.try_lock_exclusive()
        .context("Paradox watch is already running in this repository")?;
    let session_files = SessionFiles(repo.clone());
    // Holding the kernel lock proves any previous state is stale, regardless of
    // PID reuse. Stop requests authenticate the session token rather than kill PID.
    for path in [state_path(&repo), stop_path(&repo)] {
        reject_link(&path)?;
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    let (sender, receiver) = mpsc::channel();
    let signals = sender.clone();
    #[cfg(target_os = "macos")]
    let _signal_guard = crate::watch_signal::Guard::new(move || {
        let _ = signals.send(Message::Stop);
    })?;
    #[cfg(not(target_os = "macos"))]
    ctrlc::set_handler(move || {
        let _ = signals.send(Message::Stop);
    })?;
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn SetConsoleCtrlHandler(
                handler: Option<unsafe extern "system" fn(u32) -> i32>,
                add: i32,
            ) -> i32;
        }
        // A console parent/new process group can leave Ctrl+C ignored. Adding
        // the custom handler does not clear that separate inherited attribute.
        // Enable delivery only after installing our handler; a detached session
        // has no console and uses its authenticated stop mailbox instead.
        if unsafe { SetConsoleCtrlHandler(None, 0) } == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(6) {
                return Err(error).context("could not enable watch Ctrl+C delivery");
            }
        }
    }
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |event| {
        let _ = sender.send(Message::Event(event, Instant::now()));
    })?;
    let mut coverage = Coverage::new(&repo)?;
    coverage.refresh(&repo, &mut watcher)?;
    let mut state = Status {
        pid: std::process::id(),
        session: token,
        started_ms: now()?,
        loops_created: 0,
        tracked_files: coverage.files.len(),
        idle_minutes: config.watch_idle_minutes,
    };
    // Reserve the stop mailbox while starting, so stopping never needs new
    // quota allocation even if subsequent loops fill the store.
    {
        let store = LoopStore::open(repo.clone())?;
        ensure!(
            store.bytes()?.saturating_add(8192) <= store.config.store_cap_bytes,
            "watch controls do not fit the store cap"
        );
        atomic_write(&stop_path(&repo), b"")?;
    }
    persist_status(&repo, &state)?;
    let idle = idle_duration(config.watch_idle_minutes);
    println!(
        "Paradox watching {} tracked files; {}",
        state.tracked_files,
        if idle.is_some() {
            format!("stops after {} min idle", config.watch_idle_minutes)
        } else {
            "idle exit disabled".to_string()
        }
    );
    std::io::stdout().flush()?;
    let mut pending: HashMap<String, (Instant, Instant)> = HashMap::new();
    let mut recent: HashMap<String, Instant> = HashMap::new();
    let mut owned_recent: Vec<(i64, String, Instant)> = Vec::new();
    let mut bulk_owned: Vec<(i64, String)> = Vec::new();
    let mut bulk_started: Option<Instant> = None;
    let mut processed_time = Instant::now();
    let mut last_change = Instant::now();
    let mut bulk = false;
    let result = (|| -> Result<()> {
        loop {
            let clock = Instant::now();
            if let Some(threshold) = bulk_started.take() {
                bulk_owned.extend(
                    owned_recent
                        .drain(..)
                        .filter(|(_, _, event)| {
                            threshold.saturating_duration_since(*event) <= Duration::from_secs(2)
                        })
                        .map(|(id, path, _)| (id, path)),
                );
                if let Err(error) = LoopStore::open(repo.clone())
                    .and_then(|mut store| store.classify_watch_bulk(&bulk_owned, false))
                {
                    log_at(&repo.store, &format!("watch bulk promotion: {error:#}"));
                }
            }
            let file_due = |&(_, last): &(Instant, Instant)| last + Duration::from_millis(300);
            let batch_due = if bulk && !pending.is_empty() {
                Some(last_change + Duration::from_secs(2))
            } else {
                pending.values().map(file_due).min()
            };
            // Ordinary saves capture immediately after their own 300 ms
            // debounce. A later bulk threshold reclassifies only our proven
            // insertions, without delaying or merging ordinary settled saves.
            if batch_due.is_some_and(|due| clock >= due) {
                let eligible: HashSet<_> = pending
                    .iter()
                    .filter(|(_, times)| bulk || clock >= file_due(times))
                    .map(|(name, _)| name.clone())
                    .collect();
                let mut selected = eligible.clone();
                // Ignore rules may change without an index update.
                if let Err(error) = remove_ignored(&repo, &mut selected) {
                    log_at(&repo.store, &format!("watch ignore check: {error:#}"));
                    selected.clear();
                }
                let paths: Vec<_> = selected.iter().map(|name| repo.root.join(name)).collect();
                if bulk {
                    log_at(
                        &repo.store,
                        &format!(
                            "watch bulk operation: {} files; recording bulk change loops",
                            paths.len()
                        ),
                    );
                }
                let result = if paths.is_empty() {
                    Ok(crate::store::WatchCapture {
                        report: crate::store::CaptureReport::default(),
                        created: Vec::new(),
                    })
                } else {
                    Repo::discover(&repo.globals)
                        .and_then(LoopStore::open)
                        .and_then(|mut store| store.capture_watch(&paths, bulk))
                };
                match result {
                    Ok(capture) => {
                        state.loops_created += capture.report.saved as u64;
                        if bulk {
                            bulk_owned.extend(capture.created);
                        } else {
                            owned_recent.extend(capture.created.into_iter().filter_map(
                                |(id, path)| {
                                    pending.get(&path).map(|times| (id, path.clone(), times.1))
                                },
                            ));
                        }
                    }
                    Err(error) => log_at(&repo.store, &format!("watch capture: {error:#}")),
                }
                for name in eligible {
                    pending.remove(&name);
                }
                if bulk {
                    if let Err(error) = LoopStore::open(repo.clone())
                        .and_then(|mut store| store.classify_watch_bulk(&bulk_owned, true))
                    {
                        log_at(&repo.store, &format!("watch bulk collapse: {error:#}"));
                    }
                    bulk_owned.clear();
                    recent.clear();
                    bulk = false;
                }
                if let Err(error) = persist_status(&repo, &state) {
                    log_at(&repo.store, &format!("watch status: {error:#}"));
                }
                continue;
            }
            let idle_due = pending
                .is_empty()
                .then(|| idle.map(|period| last_change + period))
                .flatten();
            let deadline = batch_due.into_iter().chain(idle_due).min();
            let timeout = deadline.map(|time| time.saturating_duration_since(clock));
            match receive(&receiver, timeout)? {
                Some(Message::Stop) => break,
                Some(Message::Event(Err(error), _)) => {
                    log_at(&repo.store, &format!("watch native event: {error}"));
                }
                Some(Message::Event(Ok(event), event_time)) => {
                    if matches!(event.kind, EventKind::Access(_)) {
                        continue;
                    }
                    let mut refresh = false;
                    for path in &event.paths {
                        let parent = path.parent().map(normalized);
                        let normalized_path = parent
                            .map(|p| p.join(path.file_name().unwrap_or_default()))
                            .unwrap_or_else(|| path.clone());
                        if normalized_path == normalized(&repo.store).join("watch.stop") {
                            if fs::read_to_string(stop_path(&repo))
                                .is_ok_and(|s| s == state.session)
                            {
                                return Ok(());
                            }
                            continue;
                        }
                        if normalized_path
                            == normalized(coverage.index.parent().context("index directory")?)
                                .join(coverage.index.file_name().context("index filename")?)
                            || normalized_path == normalized(&repo.store).join("tracked.toml")
                        {
                            refresh = true;
                            continue;
                        }
                        if path.file_name().is_some_and(|name| name == ".gitignore") {
                            refresh = true;
                        }
                        if let Some(name) = coverage.relative(path) {
                            let clock = event_time;
                            processed_time = processed_time.max(clock);
                            pending
                                .entry(name.clone())
                                .and_modify(|times| {
                                    times.0 = times.0.min(clock);
                                    times.1 = times.1.max(clock);
                                })
                                .or_insert((clock, clock));
                            recent.retain(|_, when| {
                                processed_time.saturating_duration_since(*when)
                                    <= Duration::from_secs(2)
                            });
                            recent
                                .entry(name)
                                .and_modify(|when| *when = (*when).max(clock))
                                .or_insert(clock);
                            last_change = last_change.max(clock);
                            if !bulk {
                                owned_recent.retain(|(_, _, when)| {
                                    processed_time.saturating_duration_since(*when)
                                        <= Duration::from_secs(2)
                                });
                            }
                            if recent.len() > 50 && !bulk {
                                bulk = true;
                                bulk_started = Some(processed_time);
                            }
                        }
                    }
                    if refresh {
                        let previous = std::mem::take(&mut coverage.files);
                        match coverage.refresh(&repo, &mut watcher) {
                            Ok(()) => {
                                // An edit can happen after git add/track but
                                // before this control event is delivered and
                                // its new directory watch is registered. Queue
                                // only newly eligible paths after registration;
                                // the ordinary settled capture reads their
                                // latest state and shares the usual dedup.
                                let clock = event_time;
                                processed_time = processed_time.max(clock);
                                recent.retain(|_, when| {
                                    processed_time.saturating_duration_since(*when)
                                        <= Duration::from_secs(2)
                                });
                                for name in coverage.files.difference(&previous) {
                                    pending
                                        .entry(name.to_owned())
                                        .and_modify(|times| {
                                            times.0 = times.0.min(clock);
                                            times.1 = times.1.max(clock);
                                        })
                                        .or_insert((clock, clock));
                                    recent
                                        .entry(name.to_owned())
                                        .and_modify(|when| *when = (*when).max(clock))
                                        .or_insert(clock);
                                    last_change = last_change.max(clock);
                                }
                                if !bulk {
                                    owned_recent.retain(|(_, _, when)| {
                                        processed_time.saturating_duration_since(*when)
                                            <= Duration::from_secs(2)
                                    });
                                }
                                if recent.len() > 50 && !bulk {
                                    bulk = true;
                                    bulk_started = Some(processed_time);
                                }
                                drop(previous);
                            }
                            Err(error) => {
                                coverage.files = previous;
                                log_at(&repo.store, &format!("watch tracked refresh: {error:#}"));
                            }
                        }
                        state.tracked_files = coverage.files.len();
                        if let Err(error) = persist_status(&repo, &state) {
                            log_at(&repo.store, &format!("watch status: {error:#}"));
                        }
                    }
                }
                None => {
                    if idle.is_some_and(|period| last_change.elapsed() >= period)
                        && pending.is_empty()
                    {
                        break;
                    }
                }
            }
        }
        Ok(())
    })();
    // Remove only this session's controls while still owning the kernel lock.
    drop(watcher);
    drop(session_files);
    drop(lock);
    let _ = writeln!(
        std::io::stdout(),
        "Paradox watch stopped; {} loops created",
        state.loops_created
    );
    let _ = std::io::stdout().flush();
    result
}

/// Background launchers wait for a native control-file event, never poll.
pub fn wait_started(repo: &Repo, token: &str, launch: impl FnOnce() -> Result<()>) -> Result<()> {
    let (sender, receiver) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        let _ = sender.send(event);
    })?;
    watcher.watch(&repo.store, RecursiveMode::NonRecursive)?;
    launch()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(state) = read_status(repo)
            && state.session == token
        {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure!(
            !remaining.is_zero(),
            "watch background startup timed out; see <git-common-dir>/paradox/watch-output.log"
        );
        receiver
            .recv_timeout(remaining)
            .context("watch background startup timed out; see <git-common-dir>/paradox/watch-output.log")??;
    }
}

#[cfg(test)]
mod tracked_paths_tests {
    use super::TrackedPaths;
    use std::collections::HashSet;

    fn paths(names: &[&str]) -> TrackedPaths {
        TrackedPaths::from_paths(names.to_vec()).unwrap()
    }

    #[test]
    fn exact_membership_and_sorted_iteration_preserve_path_bytes() {
        let names = [
            "a",
            "a/b",
            "a-b",
            "ab",
            "a.ts",
            "space name.ts",
            "tab\tname.ts",
            "line\nname.ts",
            "return\rname.ts",
            "literal\\name.ts",
            "日本語/é.ts",
        ];
        let packed = paths(&names);
        let reference: HashSet<_> = names.into_iter().collect();
        let mut ordered: Vec<_> = reference.iter().copied().collect();
        ordered.sort_unstable();
        assert_eq!(packed.len(), reference.len());
        assert_eq!(packed.iter().collect::<Vec<_>>(), ordered);
        for name in names.into_iter().chain([
            "",
            "A",
            "a/",
            "a/b/",
            "a.t",
            "abc",
            "é.ts",
            "tab name.ts",
            "a\0b",
        ]) {
            assert_eq!(packed.contains(name), reference.contains(name), "{name:?}");
        }
        let empty = TrackedPaths::default();
        assert_eq!(empty.len(), 0);
        assert_eq!(empty.iter().count(), 0);
        assert!(!empty.contains("a"));
    }

    #[test]
    fn refresh_difference_only_returns_new_paths() {
        let before = paths(&["keep.ts", "old.ts", "line\nname.ts"]);
        let after = paths(&["keep.ts", "new.ts", "line\nname.ts", "日本語/new.ts"]);
        assert_eq!(
            after.difference(&before).collect::<Vec<_>>(),
            ["new.ts", "日本語/new.ts"]
        );
        assert_eq!(before.difference(&after).collect::<Vec<_>>(), ["old.ts"]);
        assert_eq!(after.difference(&after).count(), 0);
    }

    #[test]
    fn fifty_thousand_paths_use_only_exact_path_bytes_and_one_offset_each() {
        let names: HashSet<_> = (0..50_000).map(|n| format!("src/{n}.ts")).collect();
        let path_bytes: usize = names.iter().map(|name| name.len() + 1).sum();
        let packed = TrackedPaths::from_paths(names.iter().map(String::as_str).collect()).unwrap();
        assert_eq!(packed.len(), 50_000);
        assert_eq!(packed.arena.len(), path_bytes);
        assert_eq!(packed.starts.len(), 50_000);
        assert!(packed.arena.len() + std::mem::size_of_val(&*packed.starts) < 1_100_000);
        for n in 0..50_000 {
            assert!(packed.contains(&format!("src/{n}.ts")));
        }
        assert!(!packed.contains("src/50000.ts"));
        assert!(!packed.contains("src/1.t"));
    }

    #[test]
    fn invalid_separator_or_empty_filename_is_an_error() {
        for name in ["", "bad\0name"] {
            assert!(TrackedPaths::from_paths(vec![name]).is_err());
        }
    }

    #[test]
    fn borrowed_discovery_preserves_native_tracking_ignore_and_missing_paths() {
        let root = tempfile::tempdir().unwrap();
        let git = |arguments: &[&str]| {
            let mut command = std::process::Command::new("git");
            command.args(arguments).current_dir(root.path());
            for (name, _) in std::env::vars_os() {
                if name.to_string_lossy().starts_with("GIT_") {
                    command.env_remove(name);
                }
            }
            let output = command
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", root.path().join("absent-global"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q", "--initial-branch=main"]);
        for directory in ["nested", "ignored", "build"] {
            std::fs::create_dir(root.path().join(directory)).unwrap();
        }
        std::fs::write(
            root.path().join(".gitignore"),
            ".paradox/\nignored/\nforced.ts\n",
        )
        .unwrap();
        let cached = [
            "keep.ts",
            "missing.ts",
            "nested/space 日本語.ts",
            "forced.ts",
            "ignored/cached.ts",
            "build/cached.ts",
        ];
        for name in cached {
            std::fs::write(root.path().join(name), "base\n").unwrap();
        }
        let mut add = vec!["add", "-f", "--", ".gitignore"];
        add.extend(cached);
        git(&add);
        std::fs::remove_file(root.path().join("missing.ts")).unwrap();
        for name in ["extra.ts", "ignored/extra.ts", "raw-untracked.ts"] {
            std::fs::write(root.path().join(name), "extra\n").unwrap();
        }
        let repo = crate::repo::Repo::discover(&[
            std::ffi::OsString::from("-C"),
            root.path().as_os_str().to_owned(),
        ])
        .unwrap();
        std::fs::create_dir_all(&repo.store).unwrap();
        std::fs::write(
            repo.store.join("tracked.toml"),
            format!(
                "[worktrees]\n{} = [\"extra.ts\", \"keep.ts\", \"ignored/extra.ts\"]\n",
                repo.worktree
            ),
        )
        .unwrap();
        let mut reference = repo.tracked(&[]).unwrap();
        reference.retain(|path| !super::excluded(path) && repo.regular_file(path).is_ok());
        super::remove_ignored(&repo, &mut reference).unwrap();
        let actual = super::tracked(&repo).unwrap();
        assert_eq!(
            actual.len(),
            reference.len(),
            "Git and explicit paths must deduplicate"
        );
        assert_eq!(
            actual.iter().collect::<HashSet<_>>(),
            reference.iter().map(String::as_str).collect()
        );
        assert!(
            actual.contains("missing.ts"),
            "deleted tracked paths must remain eligible for recreation"
        );
        assert!(actual.contains("extra.ts"));
        for excluded in [
            "forced.ts",
            "ignored/cached.ts",
            "ignored/extra.ts",
            "build/cached.ts",
            "raw-untracked.ts",
        ] {
            assert!(!actual.contains(excluded), "{excluded}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn native_coverage_rejects_replaced_directory_links_but_keeps_safe_parents() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let git = |arguments: &[&str]| {
            let mut command = std::process::Command::new("git");
            command.args(arguments).current_dir(root.path());
            for (name, _) in std::env::vars_os() {
                if name.to_string_lossy().starts_with("GIT_") {
                    command.env_remove(name);
                }
            }
            let output = command
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", root.path().join("absent-global"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q", "--initial-branch=main"]);
        for directory in ["safe", "outlink", "inlink", "local_target"] {
            std::fs::create_dir(root.path().join(directory)).unwrap();
        }
        let cached = [
            "safe/missing.ts",
            "outlink/file.ts",
            "inlink/file.ts",
            "final.ts",
        ];
        for name in cached {
            std::fs::write(root.path().join(name), "base\n").unwrap();
        }
        let mut add = vec!["add", "--"];
        add.extend(cached);
        git(&add);
        for directory in ["outlink", "inlink"] {
            std::fs::remove_file(root.path().join(directory).join("file.ts")).unwrap();
            std::fs::remove_dir(root.path().join(directory)).unwrap();
        }
        std::fs::write(outside.path().join("file.ts"), "external\n").unwrap();
        std::fs::write(
            root.path().join("local_target/file.ts"),
            "untracked target\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("outlink")).unwrap();
        std::os::unix::fs::symlink(root.path().join("local_target"), root.path().join("inlink"))
            .unwrap();
        std::fs::remove_file(root.path().join("safe/missing.ts")).unwrap();
        std::fs::remove_file(root.path().join("final.ts")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("file.ts"), root.path().join("final.ts"))
            .unwrap();
        let repo = crate::repo::Repo::discover(&[
            std::ffi::OsString::from("-C"),
            root.path().as_os_str().to_owned(),
        ])
        .unwrap();
        std::fs::create_dir_all(&repo.store).unwrap();
        let mut coverage = super::Coverage::new(&repo).unwrap();
        let mut watcher =
            notify::recommended_watcher(|_: notify::Result<notify::Event>| {}).unwrap();
        coverage.refresh(&repo, &mut watcher).unwrap();
        assert!(
            !coverage
                .watched
                .contains(&super::normalized(outside.path()))
        );
        assert!(
            !coverage
                .watched
                .contains(&super::normalized(&root.path().join("local_target")))
        );
        assert!(coverage.watched.contains(&super::normalized(root.path())));
        assert!(
            coverage
                .watched
                .contains(&super::normalized(&root.path().join("safe")))
        );
        assert!(coverage.files.contains("safe/missing.ts"));
        assert!(coverage.files.contains("final.ts"));
        assert!(!coverage.files.contains("outlink/file.ts"));
        assert!(!coverage.files.contains("inlink/file.ts"));
        for rejected in [
            "outlink",
            "inlink",
            "absent",
            "safe/missing.ts",
            "../outside",
        ] {
            assert!(
                coverage.directory(std::path::Path::new(rejected)).is_none(),
                "{rejected}"
            );
        }
    }
}
