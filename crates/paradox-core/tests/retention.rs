use paradox_core::{repo::Repo, store::LoopStore};
use proptest::prelude::*;
use std::{collections::BTreeSet, fs, path::Path, process::Command};

struct Fixture {
    directory: tempfile::TempDir,
    base: i64,
}
impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            directory: tempfile::tempdir().unwrap(),
            base: paradox_core::store::now().unwrap() / 1000 - 8 * 86_400,
        };
        fixture.git(&["init", "-q", "--initial-branch=main"], None);
        for (key, value) in [
            ("user.name", "Retention Fixture"),
            ("user.email", "fixture@example.invalid"),
            ("core.autocrlf", "false"),
            ("core.hooksPath", ".git/no-hooks"),
            ("commit.gpgsign", "false"),
        ] {
            fixture.git(&["config", key, value], None);
        }
        fs::write(fixture.root().join("a.ts"), "base a\n").unwrap();
        fs::write(fixture.root().join("b.ts"), "base b\n").unwrap();
        fixture.git(&["add", "a.ts", "b.ts"], None);
        fixture.git(&["commit", "-qm", "base"], Some(fixture.base));
        fixture
    }
    fn root(&self) -> &Path {
        self.directory.path()
    }
    fn git(&self, args: &[&str], time: Option<i64>) -> String {
        let mut command = Command::new("git");
        command.current_dir(self.root()).args(args);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root().join("absent-global"));
        if let Some(time) = time {
            let date = format!("{time} +0000");
            command
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn open(&self) -> LoopStore {
        LoopStore::open(Repo::discover(&["-C".into(), self.root().as_os_str().to_owned()]).unwrap())
            .unwrap()
    }
    fn capture(&self, path: &str, bytes: &str, time: i64) -> i64 {
        fs::write(self.root().join(path), bytes).unwrap();
        let mut store = self.open();
        assert_eq!(store.capture(&[self.root().join(path)]).unwrap().saved, 1);
        let id = store
            .index
            .query_row("SELECT MAX(id) FROM loops", [], |row| row.get::<_, i64>(0))
            .unwrap();
        store
            .index
            .execute(
                "UPDATE loops SET ts=?1 WHERE id=?2",
                rusqlite::params![time, id],
            )
            .unwrap();
        id
    }
    fn commit_file(&self, path: &str, time: i64) {
        self.git(&["add", path], None);
        self.git(&["commit", "-qm", path], Some(time));
    }
    fn purge(&self, hours: u64, minimum: usize) -> usize {
        let mut store = self.open();
        store.config.retention_hours = hours;
        store.config.retention_min_loops = minimum;
        store.purge().unwrap()
    }
    fn ids(&self, path: &str) -> BTreeSet<i64> {
        self.open()
            .loops(path)
            .unwrap()
            .into_iter()
            .map(|row| row.id)
            .collect()
    }
}

#[test]
fn precommit_provenance_maps_to_last_three_file_commit_windows() {
    let f = Fixture::new();
    let mut ids = Vec::new();
    for n in 1..=8 {
        let time = f.base + n * 3600;
        ids.push(f.capture("a.ts", &format!("version {n}\n"), time * 1000 - 500));
        f.commit_file("a.ts", time);
    }
    {
        let store = f.open();
        store.pin_loop(ids[0], true).unwrap();
    }
    assert_eq!(f.purge(0, 0), 4);
    assert_eq!(
        f.ids("a.ts"),
        [ids[0], ids[5], ids[6], ids[7]].into_iter().collect()
    );
    // Every retained ordinary loop carries the HEAD *before* its file commit,
    // rather than the resulting commit ID. The oldest retained one is C6's edit.
    let current = f.git(&["rev-parse", "HEAD"], None);
    assert!(
        f.open()
            .loops("a.ts")
            .unwrap()
            .iter()
            .all(|row| row.head_sha != current.trim())
    );
}

#[test]
fn two_hour_old_uncommitted_file_survives_fifty_unrelated_commits() {
    let f = Fixture::new();
    let now = paradox_core::store::now().unwrap();
    let ids: BTreeSet<_> = (0..25)
        .map(|n| {
            f.capture(
                "a.ts",
                &format!("local edit {n}\n"),
                now - 2 * 3_600_000 + n,
            )
        })
        .collect();
    for n in 1..=50 {
        fs::write(f.root().join("b.ts"), format!("unrelated {n}\n")).unwrap();
        f.commit_file("b.ts", now / 1000 - 3600 + n);
    }
    // Disable the other two floors: per-file most-recent-commit protection
    // alone must retain these loops despite fifty newer repository commits.
    assert_eq!(f.purge(0, 0), 0);
    assert_eq!(f.ids("a.ts"), ids);
}

#[test]
fn twenty_four_hour_minimum_twenty_and_pins_are_independent_or_guarantees() {
    let f = Fixture::new();
    let baseline = f.git(&["rev-parse", "HEAD"], None).trim().to_owned();
    let mut windows = Vec::new();
    for n in 1..=8 {
        let time = f.base + n * 3600;
        let mut group = Vec::new();
        for edit in 0..5 {
            group.push(f.capture(
                "a.ts",
                &format!("old {n}/{edit}\n"),
                time * 1000 - 1000 + edit,
            ));
        }
        f.commit_file("a.ts", time);
        windows.push(group);
    }
    // With no age floor, newest twenty survives even though only fifteen
    // loops belong to the last three touched-file commits.
    let expected: BTreeSet<_> = windows[4..].iter().flatten().copied().collect();
    assert_eq!(f.purge(0, 20), 20);
    assert_eq!(f.ids("a.ts"), expected);
    let pinned = *expected.first().unwrap();
    {
        f.open().pin_loop(pinned, true).unwrap();
    }
    let now = paradox_core::store::now().unwrap();
    for n in 9..=12 {
        fs::write(
            f.root().join("a.ts"),
            format!("latest committed body {n}\n"),
        )
        .unwrap();
        f.commit_file("a.ts", now / 1000 - 3600 + (n - 9) * 600);
    }
    let mut recent = BTreeSet::new();
    for n in 0..25 {
        let id = f.capture(
            "a.ts",
            &format!("recent old-branch edit {n}\n"),
            now - 2 * 3_600_000 + n,
        );
        f.open()
            .index
            .execute(
                "UPDATE loops SET head_sha=?1 WHERE id=?2",
                rusqlite::params![baseline, id],
            )
            .unwrap();
        recent.insert(id);
    }
    f.purge(24, 20);
    let kept = f.ids("a.ts");
    assert!(recent.is_subset(&kept));
    assert!(kept.contains(&pinned));
    assert!(
        f.open()
            .loops("a.ts")
            .unwrap()
            .iter()
            .any(|row| recent.contains(&row.id) && row.ts < now - 3_600_000)
    );
    // Removing only the age floor now expires the five recent rows outside
    // the newest twenty: they predate all three newest file-commit windows.
    assert_eq!(f.purge(0, 20), 5);
    let kept = f.ids("a.ts");
    assert_eq!(recent.intersection(&kept).count(), 20);
    assert!(kept.contains(&pinned));
}

#[test]
fn pulling_fifty_other_peoples_commits_purges_no_local_work() {
    let f = Fixture::new();
    let bare = tempfile::tempdir().unwrap();
    f.git(
        &["init", "--bare", "-q", bare.path().to_str().unwrap()],
        None,
    );
    f.git(
        &["remote", "add", "origin", bare.path().to_str().unwrap()],
        None,
    );
    f.git(&["push", "-q", "-u", "origin", "main"], None);
    let now = paradox_core::store::now().unwrap();
    let ids: BTreeSet<_> = (0..25)
        .map(|n| {
            f.capture(
                "a.ts",
                &format!("uncommitted local {n}\n"),
                now - 2 * 3_600_000 + n,
            )
        })
        .collect();
    let donor = tempfile::tempdir().unwrap();
    f.git(
        &[
            "clone",
            "-q",
            "--branch",
            "main",
            bare.path().to_str().unwrap(),
            donor.path().to_str().unwrap(),
        ],
        None,
    );
    let donor_git = |args: &[&str]| {
        let mut command = Command::new("git");
        command.args(args).current_dir(donor.path());
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        let output = command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", donor.path().join("absent-global"))
            .env("GIT_AUTHOR_NAME", "Other Fixture")
            .env("GIT_AUTHOR_EMAIL", "other@example.invalid")
            .env("GIT_COMMITTER_NAME", "Other Fixture")
            .env("GIT_COMMITTER_EMAIL", "other@example.invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    donor_git(&["config", "commit.gpgsign", "false"]);
    donor_git(&["config", "core.hooksPath", ".git/no-hooks"]);
    for n in 0..50 {
        fs::write(donor.path().join("b.ts"), format!("other edit {n}\n")).unwrap();
        donor_git(&["add", "b.ts"]);
        donor_git(&["commit", "-qm", "unrelated contribution"]);
    }
    donor_git(&["push", "-q", "origin", "main"]);
    f.git(&["pull", "--ff-only", "-q"], None);
    assert_eq!(f.purge(0, 0), 0);
    assert_eq!(f.ids("a.ts"), ids);
}

#[test]
fn ambiguous_provenance_and_other_worktrees_are_kept() {
    let f = Fixture::new();
    let mut ids = Vec::new();
    for n in 1..=7 {
        let time = f.base + n * 3600;
        ids.push(f.capture("a.ts", &format!("edit {n}\n"), time * 1000 - 500));
        f.commit_file("a.ts", time);
    }
    let store = f.open();
    store
        .index
        .execute(
            "UPDATE loops SET head_sha=?1 WHERE id=?2",
            rusqlite::params!["f".repeat(40), ids[0]],
        )
        .unwrap();
    store
        .index
        .execute(
            "UPDATE loops SET worktree='other-worktree' WHERE id=?1",
            [ids[1]],
        )
        .unwrap();
    drop(store);
    f.purge(0, 0);
    assert!(f.ids("a.ts").contains(&ids[0]));
    let store = f.open();
    assert_eq!(
        store
            .index
            .query_row(
                "SELECT COUNT(*) FROM loops WHERE id=?1 AND worktree='other-worktree'",
                [ids[1]],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .index
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

proptest! {
    #[test]
    fn stripping_range_preserves_every_other_byte(prefix in prop::sample::select(vec!["//>","#>","-->","-- >",";>","%>","/*>","<!-->"]), body in prop::collection::vec(any::<u8>(),0..5000), crlf in any::<bool>(), shebang in any::<bool>()) {
        let ending=if crlf {"\r\n"} else {"\n"};let first=if shebang {format!("#!/bin/sh{ending}")} else {String::new()};
        let close=match prefix {"/*>"=>" */","<!-->"=>" -->",_=>""};
        let mut bytes=format!("{first}{prefix} message ü{close}{ending}").into_bytes();bytes.extend_from_slice(&body);
        let range=paradox_core::message::marker_range(Path::new("a.ts"),&bytes).unwrap();
        let mut stripped=bytes[..range.start].to_vec();stripped.extend_from_slice(&bytes[range.end..]);
        let mut expected=first.into_bytes();expected.extend_from_slice(&body);prop_assert_eq!(stripped,expected);
    }
}
