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

fn tracked(repo: &Repo) -> Result<HashSet<String>> {
    let mut tracked = repo.tracked(&[])?;
    tracked.retain(|path| !excluded(path) && repo.regular_file(path).is_ok());
    remove_ignored(repo, &mut tracked)?;
    Ok(tracked)
}

fn remove_ignored(repo: &Repo, paths: &mut HashSet<String>) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
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
        let paths: &HashSet<String> = paths;
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
    for path in output
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(|bytes| std::str::from_utf8(bytes).ok())
    {
        paths.remove(path);
    }
    Ok(())
}

struct Coverage {
    files: HashSet<String>,
    watched: BTreeSet<PathBuf>,
    index: PathBuf,
    root: PathBuf,
}

impl Coverage {
    fn new(repo: &Repo) -> Result<Self> {
        let index = repo.git(&["rev-parse", "--path-format=absolute", "--git-path", "index"])?;
        Ok(Self {
            files: HashSet::new(),
            watched: BTreeSet::new(),
            index: PathBuf::from(
                std::str::from_utf8(&index.stdout)?.trim_end_matches(['\r', '\n']),
            ),
            root: normalized(&repo.root),
        })
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
        let mut directories: BTreeSet<_> = files
            .iter()
            .filter_map(|name| {
                let parent = repo.root.join(name).parent()?.to_path_buf();
                if parent.is_dir() {
                    Some(normalized(&parent))
                } else {
                    None
                }
            })
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
                                        .entry(name.clone())
                                        .and_modify(|times| {
                                            times.0 = times.0.min(clock);
                                            times.1 = times.1.max(clock);
                                        })
                                        .or_insert((clock, clock));
                                    recent
                                        .entry(name.clone())
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
            "watch background startup timed out; see .paradox/watch-output.log"
        );
        receiver
            .recv_timeout(remaining)
            .context("watch background startup timed out; see .paradox/watch-output.log")??;
    }
}
