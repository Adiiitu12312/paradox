use paradox_core::blob::BlobStore;
use paradox_core::history::RewindPlan;
use paradox_core::repo::Repo;
use paradox_core::store::{LoopStore, now};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
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

fn save(store: &mut LoopStore, file: &str, bytes: &[u8]) -> i64 {
    fs::write(store.repo.root.join(file), bytes).unwrap();
    assert_eq!(store.capture(&[PathBuf::from(file)]).unwrap().saved, 1);
    store.loops(file).unwrap()[0].id
}

#[test]
fn per_file_rewind_is_undoable_and_self_writes_do_not_create_loops() {
    let root = fixture();
    let mut store = open(root.path());
    let good = b"//> fix good\r\ngood\r\n";
    let bad = b"//> fix broken\r\nbroken\r\n";
    let first = save(&mut store, "a.ts", good);
    let second = save(&mut store, "a.ts", bad);
    store.pin_loop(first, true).unwrap();
    fs::write(root.path().join("b.ts"), b"other draft\n").unwrap();
    let old = store.resolve("a.ts", &first.to_string()).unwrap();
    let new = store.resolve("a.ts", &second.to_string()).unwrap();
    let diff = store.compare("a.ts", &old, &new).unwrap();
    assert!(diff.contains("-good") && diff.contains("+broken"), "{diff}");
    let safety = store
        .rewind_file(Path::new("a.ts"), &first.to_string())
        .unwrap();
    assert_eq!(fs::read(root.path().join("a.ts")).unwrap(), good);
    assert_eq!(
        fs::read(root.path().join("b.ts")).unwrap(),
        b"other draft\n"
    );
    let records = store.loops("a.ts").unwrap();
    let backup = records.iter().find(|record| record.id == safety).unwrap();
    assert_eq!(
        (backup.kind.as_str(), backup.message.as_str()),
        ("safety", "before rewind")
    );
    assert_eq!(store.blobs.get(&backup.blob_hash).unwrap(), bad);
    assert!(
        records
            .iter()
            .find(|record| record.id == first)
            .unwrap()
            .pinned
    );
    assert_eq!(store.capture(&[]).unwrap().saved, 0);
    drop(store);
    let mut store = open(root.path());
    assert_eq!(store.capture(&[]).unwrap().saved, 0);
    store.rewind_file(Path::new("a.ts"), "undo").unwrap();
    assert_eq!(fs::read(root.path().join("a.ts")).unwrap(), bad);
    assert_eq!(
        fs::read(root.path().join("b.ts")).unwrap(),
        b"other draft\n"
    );
    assert_eq!(
        store
            .index
            .query_row("SELECT COUNT(*) FROM operation_pins", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn selectors_use_file_scoped_ids_times_messages_and_head() {
    let root = fixture();
    let mut store = open(root.path());
    let first = save(&mut store, "a.ts", b"//> fix first\nfirst\n");
    let second = save(&mut store, "a.ts", b"//> fix second\nsecond\n");
    let current = now().unwrap();
    store
        .index
        .execute(
            "UPDATE loops SET ts=?1 WHERE id=?2",
            [current - 1_200_000, first],
        )
        .unwrap();
    store
        .index
        .execute(
            "UPDATE loops SET ts=?1 WHERE id=?2",
            [current - 300_000, second],
        )
        .unwrap();
    assert_eq!(store.resolve("a.ts", "10m").unwrap().id, Some(first));
    assert_eq!(store.resolve("a.ts", "0s").unwrap().id, Some(second));
    let head = store.resolve("a.ts", "1h").unwrap();
    assert_eq!(store.version_bytes("a.ts", &head).unwrap(), b"base a\n");
    assert_eq!(store.resolve("a.ts", "fix first").unwrap().id, Some(first));
    assert!(store.resolve("a.ts", "fix").is_err());
    assert!(store.resolve("b.ts", &first.to_string()).is_err());
    assert!(store.resolve("a.ts", "9999999999999999999999h").is_err());
    assert!(store.resolve("a.ts", "").is_err());
    assert!(store.resolve("a.ts", "undo").is_err());
}

#[test]
fn tracking_is_explicit_ignored_paths_are_skipped_and_untrack_keeps_history() {
    let root = fixture();
    fs::create_dir_all(root.path().join("fresh/ignored")).unwrap();
    fs::write(root.path().join(".gitignore"), b"fresh/ignored/\n*.tmp\n").unwrap();
    for (path, bytes) in [
        ("fresh/one.ts", b"one\n".as_slice()),
        ("fresh/.env", b"secret\n"),
        ("fresh/no.tmp", b"ignored\n"),
        ("fresh/ignored/deep.ts", b"ignored\n"),
    ] {
        fs::write(root.path().join(path), bytes).unwrap();
    }
    let mut store = open(root.path());
    assert_eq!(store.capture(&[]).unwrap().saved, 0);
    assert!(!store.repo.tracked(&[]).unwrap().contains("fresh/one.ts"));
    assert_eq!(
        store.track_paths(&[PathBuf::from("fresh")], false).unwrap(),
        1
    );
    assert_eq!(store.capture(&[]).unwrap().saved, 1);
    assert_eq!(store.repo.extra_tracked().unwrap().len(), 1);
    assert!(store.loops("fresh/.env").unwrap().is_empty());
    assert_eq!(
        save(&mut store, "fresh/one.ts", b"changed\n"),
        store.loops("fresh/one.ts").unwrap()[0].id
    );
    assert_eq!(
        store.track_paths(&[PathBuf::from("fresh")], true).unwrap(),
        1
    );
    fs::write(root.path().join("fresh/one.ts"), b"untracked change\n").unwrap();
    assert_eq!(store.capture(&[]).unwrap().saved, 0);
    assert_eq!(store.loops("fresh/one.ts").unwrap().len(), 2);
    git(root.path(), &["add", "fresh/one.ts"]);
    assert_eq!(store.capture(&[]).unwrap().saved, 1);
    assert!(
        store
            .track_paths(&[PathBuf::from("../../escape")], false)
            .is_err()
    );
}

#[test]
fn tracking_the_root_registers_only_extra_paths_and_can_be_cleared() {
    let root = fixture();
    fs::write(root.path().join("fresh.ts"), b"fresh\n").unwrap();
    let mut store = open(root.path());
    assert_eq!(store.track_paths(&[PathBuf::from(".")], false).unwrap(), 1);
    assert!(store.repo.extra_tracked().unwrap().contains("fresh.ts"));
    assert!(!store.repo.extra_tracked().unwrap().contains("a.ts"));
    assert_eq!(store.track_paths(&[PathBuf::from(".")], true).unwrap(), 1);
    let tracked = store.repo.tracked(&[]).unwrap();
    assert!(tracked.contains("a.ts"));
    assert!(!tracked.contains("fresh.ts"));
}

#[test]
fn missing_files_are_recreated_and_undo_restores_absence() {
    let root = fixture();
    let mut store = open(root.path());
    let first = save(&mut store, "a.ts", b"saved\n");
    fs::remove_file(root.path().join("a.ts")).unwrap();
    let safety = store
        .rewind_file(Path::new("a.ts"), &first.to_string())
        .unwrap();
    assert_eq!(fs::read(root.path().join("a.ts")).unwrap(), b"saved\n");
    assert!(
        store
            .loops("a.ts")
            .unwrap()
            .iter()
            .find(|record| record.id == safety)
            .unwrap()
            .deleted
    );
    store.rewind_file(Path::new("a.ts"), "undo").unwrap();
    assert!(!root.path().join("a.ts").exists());
    store.rewind_file(Path::new("a.ts"), "undo").unwrap();
    assert_eq!(fs::read(root.path().join("a.ts")).unwrap(), b"saved\n");
}

#[test]
fn preview_changes_nothing_and_batch_changes_only_listed_files() {
    let root = fixture();
    let mut store = open(root.path());
    let a = save(&mut store, "a.ts", b"old a\n");
    let b = save(&mut store, "b.ts", b"old b\n");
    let cut = now().unwrap() - 60_000;
    store
        .index
        .execute("UPDATE loops SET ts=?1", [cut - 1000])
        .unwrap();
    let a_new = save(&mut store, "a.ts", b"new a\n");
    let b_new = save(&mut store, "b.ts", b"new b\n");
    fs::write(root.path().join("untracked.ts"), b"leave alone\n").unwrap();
    let preview = store.preview_at(cut).unwrap();
    assert_eq!(preview.plans.len(), 2);
    assert!(preview.skipped.is_empty());
    assert_eq!(preview.plans[0].version.id, Some(a));
    assert_eq!(preview.plans[1].version.id, Some(b));
    assert_eq!(fs::read(root.path().join("a.ts")).unwrap(), b"new a\n");
    assert_eq!(fs::read(root.path().join("b.ts")).unwrap(), b"new b\n");
    let completed = store.apply_rewinds(&preview.plans).unwrap();
    assert_eq!(completed.len(), 2);
    assert_eq!(fs::read(root.path().join("a.ts")).unwrap(), b"old a\n");
    assert_eq!(fs::read(root.path().join("b.ts")).unwrap(), b"old b\n");
    assert_eq!(
        fs::read(root.path().join("untracked.ts")).unwrap(),
        b"leave alone\n"
    );
    assert!(
        store
            .loops("a.ts")
            .unwrap()
            .iter()
            .any(|record| record.id == a_new)
    );
    assert!(
        store
            .loops("b.ts")
            .unwrap()
            .iter()
            .any(|record| record.id == b_new)
    );
    store.rewind_file(Path::new("a.ts"), "undo").unwrap();
    assert_eq!(fs::read(root.path().join("a.ts")).unwrap(), b"new a\n");
    assert_eq!(fs::read(root.path().join("b.ts")).unwrap(), b"old b\n");
}

#[test]
fn unsafe_current_content_and_mismatched_plans_are_refused_before_writing() {
    let root = fixture();
    let mut store = open(root.path());
    let id = save(&mut store, "a.ts", b"saved\n");
    fs::write(root.path().join("a.ts"), b"binary\0current").unwrap();
    assert!(
        store
            .rewind_file(Path::new("a.ts"), &id.to_string())
            .is_err()
    );
    assert_eq!(
        fs::read(root.path().join("a.ts")).unwrap(),
        b"binary\0current"
    );
    let version = store.resolve("a.ts", &id.to_string()).unwrap();
    assert!(
        store
            .apply_rewinds(&[RewindPlan {
                path: "b.ts".into(),
                version
            }])
            .is_err()
    );
    assert_eq!(fs::read(root.path().join("b.ts")).unwrap(), b"base b\n");
    let large = vec![b'x'; 2_097_153];
    fs::write(root.path().join("a.ts"), &large).unwrap();
    assert!(
        store
            .rewind_file(Path::new("a.ts"), &id.to_string())
            .is_err()
    );
    assert_eq!(fs::read(root.path().join("a.ts")).unwrap(), large);
    assert_eq!(
        store
            .index
            .query_row("SELECT COUNT(*) FROM operation_pins", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[cfg(unix)]
#[test]
fn linked_targets_are_refused_and_modes_survive_deleted_file_restore() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = fixture();
    let outside = tempfile::tempdir().unwrap();
    let mut store = open(root.path());
    fs::set_permissions(root.path().join("a.ts"), fs::Permissions::from_mode(0o751)).unwrap();
    let id = save(&mut store, "a.ts", b"executable\n");
    fs::remove_file(root.path().join("a.ts")).unwrap();
    fs::write(outside.path().join("file"), b"outside\n").unwrap();
    symlink(outside.path().join("file"), root.path().join("a.ts")).unwrap();
    assert!(
        store
            .rewind_file(Path::new("a.ts"), &id.to_string())
            .is_err()
    );
    assert_eq!(fs::read(outside.path().join("file")).unwrap(), b"outside\n");
    fs::remove_file(root.path().join("a.ts")).unwrap();
    store
        .rewind_file(Path::new("a.ts"), &id.to_string())
        .unwrap();
    assert_eq!(
        fs::metadata(root.path().join("a.ts"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
    fs::set_permissions(root.path().join("a.ts"), fs::Permissions::from_mode(0o640)).unwrap();
    store.rewind_file(Path::new("a.ts"), "HEAD").unwrap();
    assert_eq!(
        fs::metadata(root.path().join("a.ts"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
}

#[test]
fn temporary_pin_recovery_does_not_clear_permanent_user_pins() {
    let root = fixture();
    let mut store = open(root.path());
    let permanent = save(&mut store, "a.ts", b"permanent\n");
    let temporary = save(&mut store, "a.ts", b"temporary\n");
    store.pin_loop(permanent, true).unwrap();
    store.pin_loop(temporary, true).unwrap();
    store
        .index
        .execute("INSERT INTO operation_pins VALUES(?1)", [temporary])
        .unwrap();
    drop(store);
    let store = open(root.path());
    let records = store.loops("a.ts").unwrap();
    assert!(
        records
            .iter()
            .find(|record| record.id == permanent)
            .unwrap()
            .pinned
    );
    assert!(
        !records
            .iter()
            .find(|record| record.id == temporary)
            .unwrap()
            .pinned
    );
}

#[test]
fn version_one_store_migrates_without_losing_content_or_ids() {
    let root = fixture();
    let mut store = open(root.path());
    let id = save(&mut store, "a.ts", b"existing M2 data\n");
    let hash = store.loops("a.ts").unwrap()[0].blob_hash.clone();
    store.index.execute_batch("DROP TABLE operation_pins; ALTER TABLE loops DROP COLUMN mode; ALTER TABLE loops DROP COLUMN deleted; UPDATE schema_version SET version=1;").unwrap();
    drop(store);
    let store = open(root.path());
    let records = store.loops("a.ts").unwrap();
    assert_eq!(records[0].id, id);
    assert_eq!(records[0].blob_hash, hash);
    assert!(!records[0].deleted);
    assert_eq!(store.blobs.get(&hash).unwrap(), b"existing M2 data\n");
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
            .query_row("SELECT version FROM schema_version", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn git_reported_rename_keeps_history_and_stale_message_rules() {
    let root = fixture();
    let mut store = open(root.path());
    let original = save(&mut store, "a.ts", b"//> rename label\nfirst\n");
    store.capture(&[]).unwrap();
    git(root.path(), &["mv", "a.ts", "renamed.ts"]);
    let next = save(&mut store, "renamed.ts", b"//> rename label\nsecond\n");
    let records = store.loops("renamed.ts").unwrap();
    assert!(records.iter().any(|record| record.id == original));
    assert!(
        records
            .iter()
            .find(|record| record.id == next)
            .unwrap()
            .auto_label
    );
    assert!(store.loops("a.ts").unwrap().is_empty());
    store
        .rewind_file(Path::new("renamed.ts"), &original.to_string())
        .unwrap();
    assert_eq!(
        fs::read(root.path().join("renamed.ts")).unwrap(),
        b"//> rename label\nfirst\n"
    );
    assert!(!root.path().join("a.ts").exists());
    let head = store.head_version("renamed.ts").unwrap();
    assert!(!head.deleted);
    assert_eq!(
        store.version_bytes("renamed.ts", &head).unwrap(),
        b"base a\n"
    );
    store.rewind_file(Path::new("renamed.ts"), "HEAD").unwrap();
    assert_eq!(
        fs::read(root.path().join("renamed.ts")).unwrap(),
        b"base a\n"
    );
    assert!(!root.path().join("a.ts").exists());
}

#[test]
fn committed_rename_uses_gits_file_history_for_older_time_baselines() {
    let root = fixture();
    git(root.path(), &["mv", "a.ts", "renamed.ts"]);
    let date = format!("@{} +0000", (now().unwrap() - 300_000) / 1000);
    let output = Command::new("git")
        .args(["commit", "-qm", "rename"])
        .current_dir(root.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.path().join("no-global"))
        .env("GIT_AUTHOR_NAME", "Paradox Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Paradox Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let store = open(root.path());
    let old = store.resolve("renamed.ts", "1h").unwrap();
    assert!(!old.deleted);
    assert_eq!(
        store.version_bytes("renamed.ts", &old).unwrap(),
        b"base a\n"
    );
}

#[test]
fn tracked_lists_and_version_ids_are_isolated_between_worktrees() {
    let root = fixture();
    let other = tempfile::tempdir().unwrap();
    let worktree = other.path().join("worktree");
    git(
        root.path(),
        &["worktree", "add", "-b", "other", worktree.to_str().unwrap()],
    );
    let mut store = open(root.path());
    fs::write(root.path().join("extra.ts"), b"main extra\n").unwrap();
    store
        .track_paths(&[PathBuf::from("extra.ts")], false)
        .unwrap();
    let id = save(&mut store, "a.ts", b"main loop\n");
    let location = store.repo.store.clone();
    drop(store);
    let other_store = open(&worktree);
    assert_eq!(other_store.repo.store, location);
    assert!(other_store.repo.extra_tracked().unwrap().is_empty());
    assert!(other_store.resolve("a.ts", &id.to_string()).is_err());
    assert!(other_store.pin_loop(id, true).is_err());
}

#[test]
fn empty_file_presence_is_visible_and_binary_targets_are_refused() {
    let root = fixture();
    let mut store = open(root.path());
    let empty = save(&mut store, "a.ts", b"");
    fs::remove_file(root.path().join("a.ts")).unwrap();
    store
        .rewind_file(Path::new("a.ts"), &empty.to_string())
        .unwrap();
    let absent = store.resolve("a.ts", "undo").unwrap();
    let working = store.resolve("a.ts", "working").unwrap();
    let diff = store.compare("a.ts", &absent, &working).unwrap();
    assert!(diff.contains("absent -> present"), "{diff}");
    drop(store);
    fs::write(root.path().join("a.ts"), b"binary\0HEAD\n").unwrap();
    git(root.path(), &["add", "a.ts"]);
    git(root.path(), &["commit", "-qm", "binary HEAD"]);
    let mut store = open(root.path());
    save(&mut store, "a.ts", b"safe current text\n");
    assert!(store.rewind_file(Path::new("a.ts"), "HEAD").is_err());
    assert_eq!(
        fs::read(root.path().join("a.ts")).unwrap(),
        b"safe current text\n"
    );
}

fn noisy_text(seed: u32) -> Vec<u8> {
    let mut bytes = vec![0; 100_000];
    let mut hash = blake3::Hasher::new();
    hash.update(&seed.to_le_bytes());
    hash.finalize_xof().fill(&mut bytes);
    for byte in &mut bytes {
        *byte = 33 + *byte % 94;
    }
    let mut text = b"value ".to_vec();
    text.extend(bytes);
    text
}

#[test]
fn insufficient_batch_backup_space_changes_no_working_file() {
    let root = fixture();
    let mut store = open(root.path());
    let old_a = save(&mut store, "a.ts", &noisy_text(0));
    let old_b = save(&mut store, "b.ts", &noisy_text(1));
    let new_a = noisy_text(2);
    let new_b = noisy_text(3);
    save(&mut store, "a.ts", &new_a);
    save(&mut store, "b.ts", &new_b);
    let plans = [
        RewindPlan {
            path: "a.ts".into(),
            version: store.resolve("a.ts", &old_a.to_string()).unwrap(),
        },
        RewindPlan {
            path: "b.ts".into(),
            version: store.resolve("b.ts", &old_b.to_string()).unwrap(),
        },
    ];
    store.config.store_cap_bytes = 150_000;
    assert!(store.apply_rewinds(&plans).is_err());
    assert_eq!(fs::read(root.path().join("a.ts")).unwrap(), new_a);
    assert_eq!(fs::read(root.path().join("b.ts")).unwrap(), new_b);
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
    assert_eq!(store.loops("a.ts").unwrap().len(), 2);
    assert_eq!(store.loops("b.ts").unwrap().len(), 2);
}
