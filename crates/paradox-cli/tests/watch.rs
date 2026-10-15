//! Real watcher sessions confined to disposable OS-temp repositories.
use paradox_core::blob::BlobStore;
use paradox_core::{repo::Repo, store::LoopStore};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn command(root: &Path, executable: &str) -> Command {
    let mut command = Command::new(executable);
    command
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-global"))
        .env("GIT_AUTHOR_NAME", "Paradox Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Paradox Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    command
}
fn ok(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn git(root: &Path, args: &[&str]) -> String {
    ok(command(root, "git").args(args).output().unwrap())
}
fn px(root: &Path, args: &[&str]) -> String {
    ok(command(root, env!("CARGO_BIN_EXE_paradox"))
        .args(args)
        .output()
        .unwrap())
}
fn store(root: &Path) -> LoopStore {
    LoopStore::open(Repo::discover(&["-C".into(), root.as_os_str().to_owned()]).unwrap()).unwrap()
}
fn count(root: &Path) -> i64 {
    store(root)
        .index
        .query_row("SELECT COUNT(*) FROM loops", [], |r| r.get(0))
        .unwrap()
}
fn wait_for(mut condition: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(Instant::now() < until, "watch condition timed out");
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn fixture(files: usize) -> tempfile::TempDir {
    let temp = tempfile::Builder::new()
        .prefix("paradox-watch-")
        .tempdir()
        .unwrap();
    let root = temp.path();
    git(root, &["init", "-q", "--initial-branch=main"]);
    git(root, &["config", "core.autocrlf", "false"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["config", "core.hooksPath", "no-hooks"]);
    fs::write(
        root.join(".gitignore"),
        "ignored/\nnode_modules/\nbuild/\n.paradox/\n",
    )
    .unwrap();
    fs::create_dir(root.join("src")).unwrap();
    for i in 0..files {
        fs::write(root.join(format!("src/{i}.ts")), "base\n").unwrap();
    }
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "base"]);
    px(root, &["loop"]);
    assert_eq!(count(root), 0);
    temp
}
struct Session<'a> {
    root: &'a Path,
    child: Child,
}
impl<'a> Session<'a> {
    fn start(root: &'a Path) -> Self {
        Self::start_idle(root, None)
    }
    fn start_idle(root: &'a Path, idle: Option<&str>) -> Self {
        Self::start_options(root, idle, None)
    }
    fn start_options(root: &'a Path, idle: Option<&str>, limit: Option<&str>) -> Self {
        let mut cmd = command(root, env!("CARGO_BIN_EXE_paradox"));
        cmd.arg("watch")
            .stdout(Stdio::piped())
            .stderr(fs::File::create(root.join(".watch-stderr")).unwrap())
            .stdin(Stdio::null());
        if let Some(idle) = idle {
            cmd.env("PARADOX_TEST_WATCH_IDLE_MS", idle);
        }
        if let Some(limit) = limit {
            cmd.env("PARADOX_TEST_INOTIFY_LIMIT", limit);
        }
        let mut child = cmd.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let mut session = Self { root, child };
        let line = rx
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|e| {
                panic!(
                    "watch startup failed: {e}, status={:?}, stderr={}",
                    session.child.try_wait(),
                    fs::read_to_string(root.join(".watch-stderr")).unwrap_or_default()
                );
            });
        assert!(line.starts_with("Paradox watching "), "{line}");
        assert!(px(root, &["watch", "--status"]).starts_with("running pid="));
        session
    }
    fn wait_exit(&mut self) {
        wait_for(|| self.child.try_wait().unwrap().is_some());
        assert!(self.child.wait().unwrap().success());
        assert_eq!(px(self.root, &["watch", "--status"]).trim(), "not running");
    }
    fn stop(&mut self) {
        px(self.root, &["watch", "--stop"]);
        self.wait_exit();
    }
}
impl Drop for Session<'_> {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = command(self.root, env!("CARGO_BIN_EXE_paradox"))
                .args(["watch", "--stop"])
                .output();
            let until = Instant::now() + Duration::from_secs(3);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(25));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn native_saves_rename_sed_debounce_and_exclusions() {
    let temp = fixture(1);
    let root = temp.path();
    for dir in ["ignored", "node_modules", "build"] {
        fs::create_dir(root.join(dir)).unwrap();
        fs::write(root.join(dir).join("forced.ts"), "base\n").unwrap();
        git(root, &["add", "-f", &format!("{dir}/forced.ts")]);
    }
    git(root, &["commit", "-qm", "forced ignored fixtures"]);
    fs::write(root.join("ignored/explicit.ts"), "explicit base\n").unwrap();
    px(root, &["track", "ignored/explicit.ts"]);
    let mut watch = Session::start(root);
    fs::write(root.join("src/0.ts"), "//> script save\nscript body\n").unwrap();
    wait_for(|| count(root) == 1);
    assert_eq!(
        store(root).loops("src/0.ts").unwrap()[0].message,
        "script save"
    );
    fs::write(
        root.join("src/.editor-temp"),
        "//> renamed save\nrenamed body\n",
    )
    .unwrap();
    #[cfg(windows)]
    fs::remove_file(root.join("src/0.ts")).unwrap();
    fs::rename(root.join("src/.editor-temp"), root.join("src/0.ts")).unwrap();
    wait_for(|| count(root) == 2);
    assert_eq!(
        store(root).loops("src/0.ts").unwrap()[0].message,
        "renamed save"
    );
    let sed = if cfg!(target_os = "macos") {
        "!sed -i '' 's/renamed save/sed save/;s/renamed body/sed body/' src/0.ts"
    } else {
        "!sed -i 's/renamed save/sed save/;s/renamed body/sed body/' src/0.ts"
    };
    git(
        root,
        &["-c", &format!("alias.watch-edit={sed}"), "watch-edit"],
    );
    wait_for(|| count(root) == 3);
    assert_eq!(
        store(root).loops("src/0.ts").unwrap()[0].message,
        "sed save"
    );
    for i in 0..8 {
        fs::write(
            root.join("src/0.ts"),
            format!("//> settled burst\nbody {i}\n"),
        )
        .unwrap();
    }
    wait_for(|| count(root) == 4);
    for dir in ["ignored", "node_modules", "build"] {
        fs::write(root.join(dir).join("x.ts"), "//> ignored\nx\n").unwrap();
        fs::write(
            root.join(dir).join("forced.ts"),
            "//> ignored even if tracked\nx\n",
        )
        .unwrap();
    }
    fs::write(root.join("src/untracked.ts"), "//> untracked\nx\n").unwrap();
    fs::write(
        root.join("ignored/explicit.ts"),
        "//> ignored explicit path\nx\n",
    )
    .unwrap();
    fs::write(root.join(".git/ignored-probe"), "metadata\n").unwrap();
    fs::write(root.join(".paradox/ignored-probe"), "metadata\n").unwrap();
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(count(root), 4);
    assert!(px(root, &["watch", "--status"]).contains("loops_created=4"));
    assert!(
        store(root)
            .loops("src/0.ts")
            .unwrap()
            .iter()
            .all(|r| r.kind == "save")
    );
    watch.stop();
}

#[test]
fn tracked_refresh_and_rewind_suppression() {
    let temp = fixture(1);
    let root = temp.path();
    let mut watch = Session::start(root);
    fs::write(root.join("src/0.ts"), "//> first\none\n").unwrap();
    wait_for(|| count(root) == 1);
    let first = store(root).loops("src/0.ts").unwrap()[0].id;
    fs::write(root.join("src/0.ts"), "//> second\ntwo\n").unwrap();
    wait_for(|| count(root) == 2);
    px(root, &["rewind", "src/0.ts", &first.to_string()]);
    let after_rewind = count(root);
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(count(root), after_rewind);
    fs::create_dir(root.join("new-directory")).unwrap();
    fs::write(root.join("new-directory/added.ts"), "new base\n").unwrap();
    git(root, &["add", "new-directory/added.ts"]);
    // Save immediately: refresh must close the gap before native registration.
    fs::write(
        root.join("new-directory/added.ts"),
        "//> git add coverage\nadded\n",
    )
    .unwrap();
    wait_for(|| {
        store(root)
            .loops("new-directory/added.ts")
            .unwrap()
            .iter()
            .any(|r| r.message == "git add coverage")
    });
    fs::create_dir(root.join("explicit-directory")).unwrap();
    fs::write(
        root.join("explicit-directory/tracked.ts"),
        "explicit base\n",
    )
    .unwrap();
    px(root, &["track", "explicit-directory/tracked.ts"]);
    fs::write(
        root.join("explicit-directory/tracked.ts"),
        "//> explicit coverage\ntracked\n",
    )
    .unwrap();
    wait_for(|| {
        store(root)
            .loops("explicit-directory/tracked.ts")
            .unwrap()
            .iter()
            .any(|r| r.message == "explicit coverage")
    });
    watch.stop();
}

#[test]
fn checkout_sized_bulk_operation_is_one_scan_per_changed_file() {
    let temp = fixture(500);
    let root = temp.path();
    let mut watch = Session::start(root);
    for i in 0..500 {
        fs::write(
            root.join(format!("src/{i}.ts")),
            "//> branch checkout\nchanged\n",
        )
        .unwrap();
    }
    wait_for(|| count(root) == 500);
    std::thread::sleep(Duration::from_secs(3));
    let store = store(root);
    assert_eq!(
        store
            .index
            .query_row(
                "SELECT COUNT(*) FROM loops WHERE kind='scan' AND message='bulk change'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        500
    );
    assert_eq!(
        store
            .index
            .query_row("SELECT COUNT(*) FROM loops WHERE kind!='scan'", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .index
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    drop(store);
    watch.stop();
}

#[test]
fn ordinary_settled_save_is_captured_before_two_second_bulk_window() {
    let temp = fixture(1);
    let root = temp.path();
    let mut watch = Session::start(root);
    fs::write(root.join("src/0.ts"), "//> first settled save\nfirst\n").unwrap();
    let deadline = Instant::now() + Duration::from_millis(1800);
    while count(root) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(
        count(root),
        1,
        "300 ms settled save was held for the bulk window"
    );
    fs::write(root.join("src/0.ts"), "//> second settled save\nsecond\n").unwrap();
    wait_for(|| count(root) == 2);
    assert_eq!(
        store(root).loops("src/0.ts").unwrap()[0].message,
        "second settled save"
    );
    watch.stop();
}

#[test]
fn bulk_promotes_previously_settled_watcher_saves_and_keeps_latest_content() {
    let temp = fixture(51);
    let root = temp.path();
    let mut watch = Session::start(root);
    fs::write(root.join("src/0.ts"), "//> earlier settled save\nfirst\n").unwrap();
    wait_for(|| count(root) == 1);
    let latest = "//> latest bulk state\nlatest\n";
    let burst_started = Instant::now();
    fs::write(root.join("src/0.ts"), latest).unwrap();
    for i in 1..51 {
        fs::write(
            root.join(format!("src/{i}.ts")),
            "//> paced checkout\nchanged\n",
        )
        .unwrap();
    }
    let burst_elapsed = burst_started.elapsed();
    println!("bulk input: 51 changed paths in {burst_elapsed:?}");
    assert!(burst_elapsed < Duration::from_secs(2), "bulk fixture must generate its 51 changes within two seconds: {burst_elapsed:?}");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let current = store(root);
        let scans: i64 = current
            .index
            .query_row(
                "SELECT COUNT(*) FROM loops WHERE kind='scan' AND message='bulk change'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        if scans == 51 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "paced bulk: burst={burst_elapsed:?}; scans={scans}; total={}; file0={:?}; log={}",
            current
                .index
                .query_row("SELECT COUNT(*) FROM loops", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            current
                .loops("src/0.ts")
                .unwrap()
                .iter()
                .map(|row| (&row.message, &row.kind))
                .collect::<Vec<_>>(),
            fs::read_to_string(root.join(".paradox/log")).unwrap_or_default()
        );
        drop(current);
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(500));
    let store = store(root);
    assert_eq!(
        store
            .index
            .query_row(
                "SELECT MAX(n) FROM (SELECT COUNT(*) n FROM loops WHERE kind='scan' GROUP BY path)",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert!(
        store
            .index
            .query_row("SELECT COUNT(*) FROM loops", [], |r| r.get::<_, i64>(0))
            .unwrap()
            <= 52
    );
    let saved = store.loops("src/0.ts").unwrap();
    assert_eq!(
        store.blobs.get(&saved[0].blob_hash).unwrap(),
        latest.as_bytes()
    );
    drop(store);
    watch.stop();
}

#[test]
fn parallel_capture_and_editor_burst_share_dedup_and_lock() {
    let temp = fixture(1);
    let root = temp.path();
    let mut watch = Session::start(root);
    fs::write(
        root.join("src/0.ts"),
        "//> concurrent edit\nsame final bytes\n",
    )
    .unwrap();
    let children: Vec<_> = (0..20)
        .map(|_| {
            command(root, env!("CARGO_BIN_EXE_paradox"))
                .args(["loop", "src/0.ts"])
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap()
        })
        .collect();
    for _ in 0..8 {
        fs::write(
            root.join("src/.editor-concurrent-temp"),
            "//> concurrent edit\nsame final bytes\n",
        )
        .unwrap();
        #[cfg(windows)]
        fs::remove_file(root.join("src/0.ts")).unwrap();
        fs::rename(
            root.join("src/.editor-concurrent-temp"),
            root.join("src/0.ts"),
        )
        .unwrap();
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    wait_for(|| count(root) == 1);
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(count(root), 1);
    let store = store(root);
    assert_eq!(
        store
            .index
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        store
            .index
            .query_row(
                "SELECT COUNT(*) FROM loops WHERE blob_hash NOT IN (SELECT hash FROM blobs)",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    drop(store);
    watch.stop();
}

#[test]
fn settled_file_is_not_starved_by_other_files_continuing_to_change() {
    let temp = fixture(2);
    let root = temp.path();
    let mut watch = Session::start(root);
    fs::write(
        root.join("src/0.ts"),
        "//> independently settled\nfinished\n",
    )
    .unwrap();
    let running = std::sync::atomic::AtomicBool::new(true);
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| {
            let mut n = 0;
            while running.load(std::sync::atomic::Ordering::Relaxed) {
                fs::write(root.join("src/1.ts"), format!("//> still editing\n{n}\n")).unwrap();
                n += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let until = Instant::now() + Duration::from_secs(4);
        let mut captured = false;
        while Instant::now() < until {
            if !store(root).loops("src/0.ts").unwrap().is_empty() {
                captured = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        running.store(false, std::sync::atomic::Ordering::Relaxed);
        writer.join().unwrap();
        assert!(captured, "settled file was starved by unrelated edits");
    });
    wait_for(|| !store(root).loops("src/1.ts").unwrap().is_empty());
    watch.stop();
}

#[test]
fn many_changes_spread_outside_two_second_window_remain_ordinary_saves() {
    let temp = fixture(51);
    let root = temp.path();
    let mut watch = Session::start(root);
    for i in 0..51 {
        fs::write(
            root.join(format!("src/{i}.ts")),
            "//> ordinary sequential save\nchanged\n",
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(65));
    }
    wait_for(|| count(root) == 51);
    let store = store(root);
    assert_eq!(
        store
            .index
            .query_row("SELECT COUNT(*) FROM loops WHERE kind='save'", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        51
    );
    drop(store);
    watch.stop();
}

#[test]
fn singleton_force_kill_stale_recovery_and_stop_cleanup() {
    let temp = fixture(1);
    let root = temp.path();
    let mut watch = Session::start(root);
    let second = command(root, env!("CARGO_BIN_EXE_paradox"))
        .arg("watch")
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("running"));
    watch.child.kill().unwrap();
    watch.child.wait().unwrap();
    assert_eq!(px(root, &["watch", "--status"]).trim(), "not running");
    let mut recovered = Session::start(root);
    recovered.stop();
    px(root, &["watch", "--stop"]);
}

#[cfg(unix)]
#[test]
fn foreground_ctrl_c_stops_session_and_releases_instance_lock() {
    let temp = fixture(1);
    let root = temp.path();
    let mut watch = Session::start(root);
    assert!(
        Command::new("kill")
            .args(["-INT", &watch.child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    watch.wait_exit();
    let mut next = Session::start(root);
    next.stop();
}

#[cfg(all(target_os = "linux", feature = "fault-injection"))]
#[test]
fn linux_limit_warns_exact_missing_coverage_and_keeps_covered_files_live() {
    let temp = fixture(0);
    let root = temp.path();
    for dir in ["a", "z"] {
        fs::create_dir(root.join(dir)).unwrap();
        fs::write(root.join(dir).join("x.ts"), "base\n").unwrap();
    }
    git(root, &["add", "a", "z"]);
    git(root, &["commit", "-qm", "two tracked directories"]);
    // .gitignore is tracked at the root as well, so the first file directory
    // selected by the limit is root itself. Keep two slots for file directories.
    let mut watch = Session::start_options(root, None, Some("4"));
    let warning = fs::read_to_string(root.join(".watch-stderr")).unwrap();
    assert!(
        warning.contains("inotify limit 4")
            && warning.contains("NOT covered")
            && warning.contains("max_user_watches"),
        "{warning}"
    );
    assert!(
        warning.contains(&root.join("z").display().to_string()),
        "{warning}"
    );
    fs::write(root.join("a/x.ts"), "//> covered despite limit\nchanged\n").unwrap();
    fs::write(root.join("z/x.ts"), "//> not covered\nchanged\n").unwrap();
    wait_for(|| count(root) == 1);
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(count(root), 1);
    watch.stop();
}

#[cfg(feature = "fault-injection")]
#[test]
fn idle_exit_leaves_no_child_and_never_auto_starts() {
    let temp = fixture(1);
    let root = temp.path();
    let mut watch = Session::start_idle(root, Some("1400"));
    watch.wait_exit();
    px(root, &["loop", "src/0.ts"]);
    assert_eq!(px(root, &["watch", "--status"]).trim(), "not running");
}

#[test]
fn background_session_detaches_and_stops() {
    let temp = fixture(1);
    let root = temp.path();
    struct Cleanup<'a>(&'a Path);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = command(self.0, env!("CARGO_BIN_EXE_paradox"))
                .args(["watch", "--stop"])
                .output();
        }
    }
    let _cleanup = Cleanup(root);
    let child = command(root, env!("CARGO_BIN_EXE_paradox"))
        .args(["watch", "--background"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });
    let line = ok(receiver
        .recv_timeout(Duration::from_secs(20))
        .expect("background launcher retained output pipes after detaching")
        .unwrap());
    assert!(line.contains("Paradox watching "), "{line}");
    assert!(line.contains("stops after 30 min idle"), "{line}");
    let status = px(root, &["watch", "--status"]);
    assert!(status.starts_with("running pid="));
    let pid: u32 = status
        .split_ascii_whitespace()
        .find_map(|s| s.strip_prefix("pid="))
        .unwrap()
        .parse()
        .unwrap();
    fs::write(root.join("src/0.ts"), "//> detached save\nbackground\n").unwrap();
    wait_for(|| count(root) == 1);
    px(root, &["watch", "--stop"]);
    wait_for(|| px(root, &["watch", "--status"]).trim() == "not running");
    wait_for(|| !process_exists(pid));
}

fn process_exists(pid: u32) -> bool {
    #[cfg(unix)]
    {
        Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
            .is_ok_and(|output| output.status.success() && !output.stdout.is_empty())
    }
    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).lines().any(|line| {
            line.split(',')
                .nth(1)
                .is_some_and(|value| value.trim_matches('"') == pid.to_string())
        })
    }
}

fn footprint(pid: u32) -> (u64, f64) {
    #[cfg(target_os = "linux")]
    {
        let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
        let rss: u64 = status
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))
            .unwrap()
            .split_ascii_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let fields: Vec<_> = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_ascii_whitespace()
            .collect();
        let ticks = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
        let hz: f64 = ok(Command::new("getconf").arg("CLK_TCK").output().unwrap())
            .trim()
            .parse()
            .unwrap();
        (rss * 1024, ticks as f64 * 1000.0 / hz)
    }
    #[cfg(target_os = "macos")]
    {
        let output = ok(Command::new("ps")
            .args(["-o", "rss=", "-o", "time=", "-p", &pid.to_string()])
            .output()
            .unwrap());
        let mut fields = output.split_ascii_whitespace();
        let rss: u64 = fields.next().unwrap().parse().unwrap();
        let seconds = fields
            .next()
            .unwrap()
            .split(':')
            .fold(0.0, |sum, part| sum * 60.0 + part.parse::<f64>().unwrap());
        (rss * 1024, seconds * 1000.0)
    }
    #[cfg(windows)]
    {
        let script = format!(
            "$p=Get-Process -Id {pid}; Write-Output $p.WorkingSet64; Write-Output ($p.TotalProcessorTime.TotalMilliseconds.ToString([Globalization.CultureInfo]::InvariantCulture))"
        );
        let output = ok(Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .unwrap());
        let mut fields = output.split_ascii_whitespace();
        (
            fields.next().unwrap().parse().unwrap(),
            fields.next().unwrap().parse().unwrap(),
        )
    }
}

#[test]
#[ignore = "release-only ten-minute idle footprint gate on 50,000 tracked files"]
fn fifty_thousand_tracked_files_ten_minute_idle_footprint() {
    // The fixture also tracks .gitignore: 49,999 sources + 1 = 50,000.
    let temp = fixture(49_999);
    let root = temp.path();
    fs::write(
        root.join(".paradox/config.toml"),
        "watch_idle_minutes = 0\n",
    )
    .unwrap();
    let mut watch = Session::start(root);
    let (_, initial_cpu_ms) = footprint(watch.child.id());
    let started = Instant::now();
    std::thread::sleep(Duration::from_secs(600));
    assert!(watch.child.try_wait().unwrap().is_none());
    let (rss, cpu_ms) = footprint(watch.child.id());
    let cpu_percent = (cpu_ms - initial_cpu_ms) / started.elapsed().as_secs_f64() / 10.0;
    println!(
        "watch footprint: os={} files=50000 idle_seconds={:.3} rss_bytes={} cpu_percent={:.6} rss_limit_bytes={}",
        std::env::consts::OS,
        started.elapsed().as_secs_f64(),
        rss,
        cpu_percent,
        20_000_000
    );
    // Always stop the session before enforcing the measurement, including on a
    // footprint regression, so CI cannot leave a watcher behind.
    watch.stop();
    assert_eq!(count(root), 0, "idle session must not create loops");
    assert!(rss < 20_000_000, "idle RSS exceeds the 20 MB target: {rss}");
    assert!(
        cpu_percent < 0.1,
        "idle CPU must remain near zero: {cpu_percent}%"
    );
}
