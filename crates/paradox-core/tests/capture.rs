use paradox_core::blob::BlobStore;
use paradox_core::repo::Repo;
use paradox_core::store::LoopStore;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("no-global"))
        .env("GIT_AUTHOR_NAME", "Paradox Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Paradox Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fixture() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q", "--initial-branch=main"]);
    git(root.path(), &["config", "core.hooksPath", ".git/no-hooks"]);
    git(root.path(), &["config", "core.autocrlf", "false"]);
    git(root.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(root.path().join("file.ts"), "original\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "first"]);
    root
}

fn open(root: &Path) -> LoopStore {
    LoopStore::open(Repo::discover(&[OsString::from("-C"), root.as_os_str().to_owned()]).unwrap())
        .unwrap()
}

fn save(store: &mut LoopStore) -> paradox_core::store::CaptureReport {
    store.capture(&[PathBuf::from("file.ts")]).unwrap()
}

#[test]
fn saves_messages_deduplicates_and_uses_stat_cache() {
    let temp = fixture();
    let mut store = open(temp.path());
    fs::write(temp.path().join("file.ts"), "//> fix button\r\nfirst\r\n").unwrap();
    let report = save(&mut store);
    assert_eq!((report.saved, report.hashed), (1, 1));
    let report = save(&mut store);
    assert_eq!((report.saved, report.hashed), (0, 0));
    let loops = store.loops("file.ts").unwrap();
    assert_eq!(loops[0].message, "fix button");
    assert!(!loops[0].auto_label);
    assert_eq!(
        store.blobs.get(&loops[0].blob_hash).unwrap(),
        b"//> fix button\r\nfirst\r\n"
    );
    assert_eq!(loops[0].head_sha.len(), 40);
    // Explicitly change mtime but not bytes: hashing is required, no new loop.
    let file = fs::OpenOptions::new()
        .write(true)
        .open(temp.path().join("file.ts"))
        .unwrap();
    file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(2))
        .unwrap();
    let report = save(&mut store);
    assert_eq!((report.saved, report.hashed), (0, 1));
    fs::write(
        temp.path().join("file.ts"),
        "//> fix button\r\nsecond changed\r\n",
    )
    .unwrap();
    assert_eq!(save(&mut store).saved, 1);
    assert!(store.loops("file.ts").unwrap()[0].auto_label);
    assert_eq!(
        store
            .index
            .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    assert!(
        store
            .repo
            .store
            .join("index.sqlite3-wal")
            .metadata()
            .is_ok_and(|m| m.len() == 0)
    );
}

#[test]
fn closed_comment_messages_are_saved_whole_and_become_stale() {
    let temp = fixture();
    for (path, line) in [
        ("style.css", "/*> adjust spacing */"),
        ("page.html", "<!--> adjust layout -->"),
        ("query.sql", "-- > adjust query"),
    ] {
        let bytes = format!("{line}\r\nfirst\r\n");
        fs::write(temp.path().join(path), &bytes).unwrap();
        git(temp.path(), &["add", path]);
        let mut store = open(temp.path());
        assert_eq!(store.capture(&[PathBuf::from(path)]).unwrap().saved, 1);
        let loops = store.loops(path).unwrap();
        assert!(!loops[0].auto_label);
        assert_eq!(
            store.blobs.get(&loops[0].blob_hash).unwrap(),
            bytes.as_bytes()
        );
        let changed = format!("{line}\r\nsecond changed\r\n");
        fs::write(temp.path().join(path), &changed).unwrap();
        assert_eq!(store.capture(&[PathBuf::from(path)]).unwrap().saved, 1);
        assert!(store.loops(path).unwrap()[0].auto_label);
        assert_eq!(
            fs::read(temp.path().join(path)).unwrap(),
            changed.as_bytes()
        );
    }
}

#[test]
fn ignores_untracked_directories_secrets_binary_large_and_links() {
    let temp = fixture();
    for (name, bytes) in [
        (".env.production", b"PASSWORD=private".as_slice()),
        ("private.pem", b"private"),
        ("id_rsa_work", b"private"),
        ("file.bin", b"start\0binary"),
    ] {
        fs::write(temp.path().join(name), bytes).unwrap();
        git(temp.path(), &["add", name]);
    }
    fs::write(temp.path().join("huge.txt"), vec![b'x'; 2_097_153]).unwrap();
    git(temp.path(), &["add", "huge.txt"]);
    fs::create_dir_all(temp.path().join("new/node_modules/deep")).unwrap();
    fs::write(temp.path().join("new/node_modules/deep/noise.txt"), "noise").unwrap();
    let mut store = open(temp.path());
    assert_eq!(store.capture(&[]).unwrap().saved, 0);
    assert_eq!(store.capture(&[PathBuf::from("new")]).unwrap().saved, 0);
    assert!(store.loops(".env.production").unwrap().is_empty());
    assert_eq!(
        store
            .index
            .query_row("SELECT COUNT(*) FROM loops", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(store.repo.relative(Path::new("../../outside")).is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("file.ts", temp.path().join("linked")).unwrap();
        git(temp.path(), &["add", "linked"]);
        assert_eq!(store.capture(&[PathBuf::from("linked")]).unwrap().saved, 0);
    }
    git(temp.path(), &["add", "-A"]);
    let indexed = Command::new("git")
        .current_dir(temp.path())
        .args(["ls-files", ".paradox"])
        .output()
        .unwrap();
    assert!(
        indexed.stdout.is_empty(),
        "store metadata must not enter the index"
    );
}

#[test]
fn scan_only_captures_dirty_tracked_files_and_no_commits_is_supported() {
    let temp = fixture();
    let mut store = open(temp.path());
    assert_eq!(store.capture(&[]).unwrap().saved, 0);
    fs::write(temp.path().join("file.ts"), "//> scan message\nmodified\n").unwrap();
    assert_eq!(store.capture(&[]).unwrap().saved, 1);
    assert_eq!(store.loops("file.ts").unwrap()[0].kind, "scan");
    assert_eq!(store.capture(&[]).unwrap().hashed, 0);
    drop(store);
    let unborn = tempfile::tempdir().unwrap();
    git(unborn.path(), &["init", "-q"]);
    fs::write(unborn.path().join("file.ts"), "first\n").unwrap();
    git(unborn.path(), &["add", "file.ts"]);
    let mut store = open(unborn.path());
    assert_eq!(save(&mut store).saved, 1);
    assert!(store.loops("file.ts").unwrap()[0].head_sha.is_empty());
}

#[test]
fn large_scan_keeps_baseline_and_detects_changes_after_warmup() {
    let temp = fixture();
    for n in 0..600 {
        fs::write(temp.path().join(format!("tracked-{n}.ts")), b"base\n").unwrap();
    }
    git(temp.path(), &["add", "-A"]);
    git(temp.path(), &["commit", "-qm", "tracked scan fixture"]);
    let mut store = open(temp.path());
    assert_eq!(store.capture(&[]).unwrap().saved, 0);
    assert_eq!(store.capture(&[]).unwrap().hashed, 0);
    fs::write(
        temp.path().join("tracked-599.ts"),
        b"//> last worker\nchanged\n",
    )
    .unwrap();
    let report = store.capture(&[]).unwrap();
    assert_eq!((report.saved, report.hashed), (1, 1));
    assert_eq!(
        store.loops("tracked-599.ts").unwrap()[0].message,
        "last worker"
    );
    assert_eq!(store.capture(&[]).unwrap().hashed, 0);
}

#[test]
#[cfg(unix)]
fn literal_backslash_paths_do_not_select_a_different_file() {
    let root = fixture();
    fs::create_dir(root.path().join("nested")).unwrap();
    fs::write(root.path().join("nested/file.ts"), b"different file\n").unwrap();
    fs::write(root.path().join(r"nested\file.ts"), b"literal backslash\n").unwrap();
    git(root.path(), &["add", "-A"]);
    let mut store = open(root.path());
    let report = store.capture(&[PathBuf::from(r"nested\file.ts")]).unwrap();
    assert_eq!(report.saved, 1);
    let loops = store.loops(r"nested\file.ts").unwrap();
    assert_eq!(loops.len(), 1);
    assert!(store.loops("nested/file.ts").unwrap().is_empty());
    assert_eq!(
        store.blobs.get(&loops[0].blob_hash).unwrap(),
        b"literal backslash\n"
    );
}

#[test]
#[cfg(unix)]
fn ambiguous_repository_paths_are_rejected_before_store_creation() {
    let root = tempfile::Builder::new()
        .prefix("paradox\n")
        .tempdir()
        .unwrap();
    git(root.path(), &["init", "-q"]);
    let repo = Repo::discover(&[OsString::from("-C"), root.path().as_os_str().to_owned()]);
    assert!(repo.is_err());
    assert!(!root.path().join(".paradox").exists());
}

#[test]
#[cfg(unix)]
fn rejected_store_link_cannot_log_outside_the_repository() {
    let temp = fixture();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("log"), b"untouched\n").unwrap();
    let linked = temp.path().join(".paradox");
    std::os::unix::fs::symlink(outside.path(), &linked).unwrap();
    let repo = Repo::discover(&[OsString::from("-C"), temp.path().as_os_str().to_owned()]).unwrap();
    assert!(LoopStore::open(repo).is_err());
    paradox_core::store::log_at(&linked, "store failed");
    assert_eq!(
        fs::read(outside.path().join("log")).unwrap(),
        b"untouched\n"
    );
    assert!(!outside.path().join("lock").exists());
    assert!(!outside.path().join("index.sqlite3").exists());
}

#[test]
fn blob_integrity_orphan_cleanup_and_schema_guard() {
    let temp = fixture();
    let mut store = open(temp.path());
    assert_eq!(save(&mut store).saved, 1);
    let hash = blake3::hash(b"orphan").to_hex().to_string();
    store.blobs.put(&hash, b"orphan").unwrap();
    let pending = store
        .blobs
        .path(&hash)
        .unwrap()
        .parent()
        .unwrap()
        .join(".pending-abandoned");
    fs::write(&pending, b"partial").unwrap();
    store.gc().unwrap();
    assert!(!store.blobs.exists(&hash).unwrap());
    assert!(!pending.exists());
    let saved = store.loops("file.ts").unwrap();
    fs::write(store.blobs.path(&saved[0].blob_hash).unwrap(), b"corrupt").unwrap();
    assert!(store.blobs.get(&saved[0].blob_hash).is_err());
    assert!(store.blobs.path("../../escape").is_err());
    store
        .index
        .execute("UPDATE schema_version SET version=999", [])
        .unwrap();
    let repo = store.repo.clone();
    drop(store);
    assert!(LoopStore::open(repo).is_err());
}

fn text(seed: u32, len: usize) -> Vec<u8> {
    let mut bytes = vec![0; len];
    let mut hasher = blake3::Hasher::new();
    hasher.update(&seed.to_le_bytes());
    hasher.finalize_xof().fill(&mut bytes);
    for b in &mut bytes {
        *b = 33 + *b % 94;
    }
    bytes
}

#[test]
fn evicts_oldest_unpinned_and_keeps_pins_with_bounded_storage() {
    let temp = fixture();
    let mut store = open(temp.path());
    store.config.store_cap_bytes = 400_000;
    fs::write(temp.path().join("file.ts"), text(0, 120_000)).unwrap();
    assert_eq!(save(&mut store).saved, 1);
    let pinned = store.loops("file.ts").unwrap()[0].id;
    store
        .index
        .execute("UPDATE loops SET pinned=1 WHERE id=?1", [pinned])
        .unwrap();
    fs::write(temp.path().join("file.ts"), text(1, 120_000)).unwrap();
    assert_eq!(save(&mut store).saved, 1);
    let oldest_unpinned = store.loops("file.ts").unwrap()[0].id;
    for n in 2..8 {
        fs::write(temp.path().join("file.ts"), text(n, 120_000)).unwrap();
        assert_eq!(save(&mut store).saved, 1);
        assert!(store.bytes().unwrap() <= store.config.store_cap_bytes);
    }
    let loops = store.loops("file.ts").unwrap();
    assert!(loops.iter().any(|l| l.id == pinned && l.pinned));
    assert!(!loops.iter().any(|l| l.id == oldest_unpinned));
    assert_eq!(
        store.blobs.get(&loops[0].blob_hash).unwrap(),
        text(7, 120_000)
    );
}

#[test]
fn worktrees_share_blobs_but_not_file_state() {
    let temp = fixture();
    let other = tempfile::tempdir().unwrap();
    let worktree = other.path().join("worktree");
    git(
        temp.path(),
        &["worktree", "add", "-b", "other", worktree.to_str().unwrap()],
    );
    let mut first = open(temp.path());
    let root = first.repo.store.clone();
    save(&mut first);
    drop(first);
    let mut second = open(&worktree);
    assert_eq!(second.repo.store, root);
    assert!(second.loops("file.ts").unwrap().is_empty());
    assert_eq!(save(&mut second).saved, 1);
    assert_eq!(
        second
            .index
            .query_row("SELECT COUNT(*) FROM blobs", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
