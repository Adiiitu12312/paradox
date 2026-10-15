use paradox_core::{blob::BlobStore, repo::Repo, store::LoopStore};
use rusqlite::OptionalExtension;
use std::{fs, path::Path, process::Command};
fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("no-global"))
        .env("GIT_AUTHOR_NAME", "Store Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Store Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-q", "--initial-branch=main"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["config", "core.hooksPath", ".git/no-hooks"]);
    fs::write(root.join("a.ts"), "base\n").unwrap();
    git(root, &["add", "a.ts"]);
    git(root, &["commit", "-qm", "base"]);
    directory
}
fn repo(root: &Path) -> Repo {
    Repo::discover(&["-C".into(), root.as_os_str().to_owned()]).unwrap()
}
#[test]
fn common_store_needs_no_exclusion_and_legacy_migration_keeps_ids_and_bytes() {
    let directory = fixture();
    let root = directory.path();
    let expected = root.join(".git/paradox");
    let exclude = root.join(".git/info/exclude");
    let before = fs::read(&exclude).unwrap_or_default();
    fs::write(root.join("a.ts"), "//> retained migration\nchanged\n").unwrap();
    let id = {
        let mut store = LoopStore::open(repo(root)).unwrap();
        store.capture(&[]).unwrap();
        assert_eq!(
            fs::canonicalize(&store.repo.store).unwrap(),
            fs::canonicalize(&expected).unwrap()
        );
        store
            .index
            .execute("UPDATE schema_version SET version=2", [])
            .unwrap();
        store
            .index
            .execute("UPDATE sqlite_sequence SET seq=1000 WHERE name='loops'", [])
            .unwrap();
        store.loops("a.ts").unwrap()[0].id
    };
    assert_eq!(fs::read(&exclude).unwrap_or_default(), before);
    fs::rename(&expected, root.join(".paradox")).unwrap();
    assert!(
        LoopStore::open(repo(root)).is_err(),
        "must not silently abandon legacy history"
    );
    paradox_core::migration::migrate(&repo(root)).unwrap();
    assert!(!root.join(".paradox").exists());
    let mut store = LoopStore::open(repo(root)).unwrap();
    let loops = store.loops("a.ts").unwrap();
    assert_eq!(loops[0].id, id);
    assert_eq!(
        store.blobs.get(&loops[0].blob_hash).unwrap(),
        b"//> retained migration\nchanged\n"
    );
    assert_eq!(
        store
            .index
            .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
            .optional()
            .unwrap(),
        None
    );
    fs::write(root.join("a.ts"), "after migration\n").unwrap();
    store.capture(&[]).unwrap();
    assert!(store.loops("a.ts").unwrap()[0].id > 1000);
}
#[test]
fn migration_refuses_two_stores_without_removing_either() {
    let directory = fixture();
    let root = directory.path();
    drop(LoopStore::open(repo(root)).unwrap());
    fs::create_dir(root.join(".paradox")).unwrap();
    fs::write(root.join(".paradox/precious"), b"preserve me").unwrap();
    assert!(paradox_core::migration::migrate(&repo(root)).is_err());
    assert_eq!(
        fs::read(root.join(".paradox/precious")).unwrap(),
        b"preserve me"
    );
    assert!(root.join(".git/paradox/index.sqlite3").is_file());
}

fn legacy_fixture() -> (tempfile::TempDir, i64, Vec<u8>) {
    let directory = fixture();
    let root = directory.path();
    let bytes = b"//> legacy version\npreserve every byte\n".to_vec();
    fs::write(root.join("a.ts"), &bytes).unwrap();
    let id = {
        let mut store = LoopStore::open(repo(root)).unwrap();
        store.capture(&[]).unwrap();
        store.loops("a.ts").unwrap()[0].id
    };
    fs::rename(root.join(".git/paradox"), root.join(".paradox")).unwrap();
    (directory, id, bytes)
}

#[test]
fn migration_refuses_active_legacy_store_or_watch_without_losing_history() {
    for name in ["lock", "watch.lock"] {
        let (directory, id, bytes) = legacy_fixture();
        let root = directory.path();
        let held = fs::OpenOptions::new().read(true).write(true).create(true)
            .truncate(false).open(root.join(".paradox").join(name)).unwrap();
        fs2::FileExt::lock_exclusive(&held).unwrap();
        let error = paradox_core::migration::migrate(&repo(root)).unwrap_err();
        let expected = if name == "lock" { "Legacy store is in use" } else { "Stop paradox watch" };
        assert!(error.to_string().contains(expected), "{error:#}");
        assert!(root.join(".paradox/index.sqlite3").is_file());
        assert!(!root.join(".git/paradox").exists());
        assert!(LoopStore::open(repo(root)).is_err());
        drop(held);
        paradox_core::migration::migrate(&repo(root)).unwrap();
        let store = LoopStore::open(repo(root)).unwrap();
        let loops = store.loops("a.ts").unwrap();
        assert_eq!(loops[0].id, id);
        assert_eq!(store.blobs.get(&loops[0].blob_hash).unwrap(), bytes);
        assert!(!root.join(".paradox").exists());
    }
}

#[test]
fn simultaneous_migrations_share_one_gate_and_preserve_ids_and_bytes() {
    let (directory, id, bytes) = legacy_fixture();
    let root = directory.path();
    let first = repo(root);
    let second = first.clone();
    let start = std::sync::Barrier::new(3);
    std::thread::scope(|scope| {
        let a = scope.spawn(|| { start.wait(); paradox_core::migration::migrate(&first) });
        let b = scope.spawn(|| { start.wait(); paradox_core::migration::migrate(&second) });
        start.wait();
        a.join().unwrap().unwrap();
        b.join().unwrap().unwrap();
    });
    assert!(!root.join(".paradox").exists());
    assert!(root.join(".git/paradox-migration.lock").is_file());
    let store = LoopStore::open(repo(root)).unwrap();
    let loops = store.loops("a.ts").unwrap();
    assert_eq!(loops.len(), 1);
    assert_eq!(loops[0].id, id);
    assert_eq!(store.blobs.get(&loops[0].blob_hash).unwrap(), bytes);
    assert_eq!(store.index.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
}

#[test]
fn deletion_has_no_blob_reappearance_keeps_history_and_rewind_restores_before_deletion() {
    let directory = fixture();
    let root = directory.path();
    let mut store = LoopStore::open(repo(root)).unwrap();
    fs::write(root.join("a.ts"), "//> keep this\noriginal edit\n").unwrap();
    store.capture(&[]).unwrap();
    let saved = store.loops("a.ts").unwrap()[0].id;
    let blobs: i64 = store
        .index
        .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get(0))
        .unwrap();
    fs::remove_file(root.join("a.ts")).unwrap();
    assert_eq!(store.capture(&[]).unwrap().saved, 1);
    let deleted = store.loops("a.ts").unwrap()[0].clone();
    assert_eq!(deleted.kind, "deleted");
    assert!(deleted.deleted);
    assert!(deleted.blob_hash.is_empty());
    assert_eq!(
        store
            .index
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        blobs
    );
    assert_eq!(store.capture(&[]).unwrap().saved, 0);
    fs::write(root.join("a.ts"), "returned\n").unwrap();
    assert_eq!(store.capture(&[]).unwrap().saved, 1);
    assert!(
        store
            .loops("a.ts")
            .unwrap()
            .iter()
            .any(|item| item.id == saved)
    );
    store
        .rewind_file(Path::new("a.ts"), &saved.to_string())
        .unwrap();
    assert_eq!(
        fs::read(root.join("a.ts")).unwrap(),
        b"//> keep this\noriginal edit\n"
    );
}
#[test]
fn cap_protects_every_files_newest_loop_and_warns_when_it_cannot_fit() {
    let directory = fixture();
    let root = directory.path();
    fs::write(root.join("b.ts"), "base\n").unwrap();
    git(root, &["add", "b.ts"]);
    git(root, &["commit", "-qm", "second file"]);
    let mut store = LoopStore::open(repo(root)).unwrap();
    for n in 0..4 {
        fs::write(root.join("a.ts"), format!("a version {n}\n")).unwrap();
        store.capture(&[]).unwrap();
    }
    fs::write(root.join("b.ts"), "newest b\n").unwrap();
    store.capture(&[]).unwrap();
    let a = store.loops("a.ts").unwrap()[0].id;
    let b = store.loops("b.ts").unwrap()[0].id;
    store.config.store_cap_bytes = 1;
    store.gc().unwrap();
    assert!(store.loops("a.ts").unwrap().iter().any(|item| item.id == a));
    assert!(store.loops("b.ts").unwrap().iter().any(|item| item.id == b));
    assert!(
        fs::read_to_string(store.repo.store.join("log"))
            .unwrap()
            .contains("newest per-file")
    );
}
