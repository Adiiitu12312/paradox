use paradox_core::repo::Repo;
use paradox_core::store::LoopStore;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn command(exe: &str, root: &Path) -> Command {
    let mut command = Command::new(exe);
    command
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("no-global"))
        .env("GIT_AUTHOR_NAME", "Paradox Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Paradox Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .env("NO_COLOR", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    command
}

fn git(root: &Path, args: &[&str]) {
    let output = command("git", root).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn paradox(root: &Path, args: &[&str]) -> Output {
    command(env!("CARGO_BIN_EXE_paradox"), root)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn success(root: &Path, args: &[&str]) -> String {
    let output = paradox(root, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q", "--initial-branch=main"]);
    git(root.path(), &["config", "core.hooksPath", ".git/no-hooks"]);
    git(root.path(), &["config", "core.autocrlf", "false"]);
    git(root.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(root.path().join("a.ts"), b"base a\n").unwrap();
    fs::write(root.path().join("b.ts"), b"base b\n").unwrap();
    git(root.path(), &["add", "-A"]);
    git(root.path(), &["commit", "-qm", "base"]);
    root
}

fn open(root: &Path) -> LoopStore {
    LoopStore::open(Repo::discover(&[OsString::from("-C"), root.as_os_str().to_owned()]).unwrap())
        .unwrap()
}

fn save(root: &Path, bytes: &[u8]) -> i64 {
    fs::write(root.join("a.ts"), bytes).unwrap();
    success(root, &["loop", "a.ts"]);
    open(root).loops("a.ts").unwrap()[0].id
}

#[test]
fn history_compare_rewind_undo_pin_and_head_work_end_to_end() {
    let root = fixture();
    let a = save(root.path(), b"//> fix first\nfirst\n");
    let b = save(root.path(), b"//> fix second\nsecond\n");
    let listing = success(root.path(), &["loops", "a.ts"]);
    assert!(
        listing.contains("fix first") && listing.contains("fix second") && listing.contains("HEAD")
    );
    assert!(listing.find(&format!("{b} ")).unwrap() < listing.find(&format!("{a} ")).unwrap());
    let diff = success(
        root.path(),
        &["compare", "a.ts", &a.to_string(), &b.to_string()],
    );
    assert!(
        diff.contains("-first") && diff.contains("+second"),
        "{diff}"
    );
    assert!(!diff.contains('\x1b'));
    assert!(
        !paradox(root.path(), &["compare", "a.ts", "fix"])
            .status
            .success()
    );
    success(root.path(), &["pin", &a.to_string()]);
    assert!(success(root.path(), &["loops", "a.ts"]).contains("pinned"));
    fs::write(root.path().join("b.ts"), b"other draft\n").unwrap();
    success(root.path(), &["rewind", "a.ts", &a.to_string()]);
    assert_eq!(
        fs::read(root.path().join("a.ts")).unwrap(),
        b"//> fix first\nfirst\n"
    );
    assert_eq!(
        fs::read(root.path().join("b.ts")).unwrap(),
        b"other draft\n"
    );
    success(root.path(), &["rewind", "a.ts", "undo"]);
    assert_eq!(
        fs::read(root.path().join("a.ts")).unwrap(),
        b"//> fix second\nsecond\n"
    );
    success(root.path(), &["rewind", "a.ts", "HEAD"]);
    assert_eq!(fs::read(root.path().join("a.ts")).unwrap(), b"base a\n");
    success(root.path(), &["unpin", &a.to_string()]);
    assert!(
        !open(root.path())
            .loops("a.ts")
            .unwrap()
            .iter()
            .find(|record| record.id == a)
            .unwrap()
            .pinned
    );
}

#[test]
fn track_does_not_stage_and_untrack_only_removes_explicit_membership() {
    let root = fixture();
    fs::write(root.path().join("fresh.css"), b"/*> new style */\na {}\n").unwrap();
    success(root.path(), &["loop", "fresh.css"]);
    assert!(open(root.path()).loops("fresh.css").unwrap().is_empty());
    success(root.path(), &["track", "fresh.css"]);
    assert_eq!(
        open(root.path()).loops("fresh.css").unwrap()[0].message,
        "new style"
    );
    let tracked = command("git", root.path())
        .args(["ls-files", "--", "fresh.css"])
        .output()
        .unwrap();
    assert!(tracked.stdout.is_empty(), "track must not stage files");
    success(root.path(), &["untrack", "fresh.css"]);
    fs::write(root.path().join("fresh.css"), b"changed\n").unwrap();
    success(root.path(), &["loop"]);
    assert_eq!(open(root.path()).loops("fresh.css").unwrap().len(), 1);
    git(root.path(), &["add", "fresh.css"]);
    success(root.path(), &["loop", "fresh.css"]);
    assert_eq!(open(root.path()).loops("fresh.css").unwrap().len(), 2);
}

#[test]
fn batch_preview_requires_confirmation_and_yes_restores_each_file() {
    let root = fixture();
    let old = save(root.path(), b"old a\n");
    let current = save(root.path(), b"new a\n");
    fs::write(root.path().join("b.ts"), b"new b\n").unwrap();
    success(root.path(), &["loop", "b.ts"]);
    let at = paradox_core::history::clock_time("00:00").unwrap();
    let store = open(root.path());
    store
        .index
        .execute("UPDATE loops SET ts=?1", [at + 172_800_000])
        .unwrap();
    store
        .index
        .execute("UPDATE loops SET ts=?1 WHERE id=?2", [at - 1000, old])
        .unwrap();
    drop(store);
    let output = paradox(root.path(), &["rewind", "--at", "00:00"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Dry run: 2 file(s)"));
    assert_eq!(fs::read(root.path().join("a.ts")).unwrap(), b"new a\n");
    assert_eq!(fs::read(root.path().join("b.ts")).unwrap(), b"new b\n");
    let output = success(root.path(), &["rewind", "--at", "00:00", "--yes"]);
    assert!(output.contains("safety loop"));
    assert_eq!(fs::read(root.path().join("a.ts")).unwrap(), b"old a\n");
    assert_eq!(fs::read(root.path().join("b.ts")).unwrap(), b"base b\n");
    assert!(
        open(root.path())
            .loops("a.ts")
            .unwrap()
            .iter()
            .any(|record| record.id == current)
    );
}

#[test]
fn help_never_initializes_a_repository_or_store() {
    let root = tempfile::tempdir().unwrap();
    for name in [
        "loops", "compare", "rewind", "pin", "unpin", "track", "untrack",
    ] {
        let output = paradox(root.path(), &[name, "--help"]);
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(!root.path().join(".git").exists());
    assert!(!root.path().join(".git/paradox").exists());
}

#[cfg(feature = "fault-injection")]
#[test]
fn interrupted_rewinds_keep_undo_data_recover_leases_and_suppress_self_writes() {
    for point in [
        "after_rewind_safety",
        "before_rewind_write",
        "after_rewind_write",
    ] {
        let root = fixture();
        let id = save(root.path(), b"good\n");
        save(root.path(), b"bad\n");
        success(root.path(), &["pin", &id.to_string()]);
        let result = command(env!("CARGO_BIN_EXE_paradox"), root.path())
            .args(["rewind", "a.ts", &id.to_string()])
            .env("PARADOX_TEST_CRASH_AT", point)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!result.success(), "{point} did not interrupt");
        assert_eq!(
            fs::read(root.path().join("a.ts")).unwrap(),
            if point == "after_rewind_write" {
                b"good\n".as_slice()
            } else {
                b"bad\n".as_slice()
            }
        );
        let mut store = open(root.path());
        assert_eq!(
            store
                .index
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert_eq!(
            store
                .index
                .query_row("SELECT COUNT(*) FROM operation_pins", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(
            store
                .loops("a.ts")
                .unwrap()
                .iter()
                .find(|record| record.id == id)
                .unwrap()
                .pinned
        );
        assert_eq!(
            store.capture(&[]).unwrap().saved,
            0,
            "{point} created a loop of Paradox's own write"
        );
        assert!(
            store
                .loops("a.ts")
                .unwrap()
                .iter()
                .any(|record| record.kind == "safety")
        );
        drop(store);
        if point == "after_rewind_write" {
            let other = save(root.path(), b"external change\n");
            let returned = save(root.path(), b"good\n");
            assert!(
                returned > other,
                "a stale writing marker suppressed a later external save"
            );
        }
        success(root.path(), &["rewind", "a.ts", "undo"]);
        assert_eq!(fs::read(root.path().join("a.ts")).unwrap(), b"bad\n");
    }
}
