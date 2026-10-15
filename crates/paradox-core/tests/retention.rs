use paradox_core::{repo::Repo, store::LoopStore};
use proptest::prelude::*;
use std::fs;
use std::path::Path;
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-global"))
        .env("GIT_AUTHOR_NAME", "Paradox Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Paradox Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn open(root: &Path) -> LoopStore {
    LoopStore::open(Repo::discover(&["-C".into(), root.as_os_str().to_owned()]).unwrap()).unwrap()
}

#[test]
fn retention_keeps_boundary_pins_dirty_paths_and_fewer_than_three_commits() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    git(root, &["init", "-q", "--initial-branch=main"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["config", "core.autocrlf", "false"]);
    fs::write(root.join("a.ts"), "a0\n").unwrap();
    fs::write(root.join("b.ts"), "b0\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "base"]);
    let mut pinned = 0;
    for n in 1..=6 {
        fs::write(root.join("a.ts"), format!("a{n}\n")).unwrap();
        fs::write(root.join("b.ts"), format!("b{n}\n")).unwrap();
        {
            let mut store = open(root);
            store.capture(&[]).unwrap();
            if n == 1 {
                pinned = store.loops("a.ts").unwrap()[0].id;
                store.pin_loop(pinned, true).unwrap();
            }
        }
        git(root, &["add", "a.ts", "b.ts"]);
        git(root, &["commit", "-qm", &format!("commit {n}")]);
        if n == 1 {
            let mut store = open(root);
            assert_eq!(store.purge().unwrap(), 0);
            assert_eq!(store.loops("a.ts").unwrap().len(), 1);
        }
    }
    let recent = git(
        root,
        &["rev-list", "--first-parent", "--max-count=3", "HEAD"],
    );
    fs::write(root.join("b.ts"), "uncommitted b\n").unwrap();
    let mut store = open(root);
    store.capture(&[]).unwrap();
    assert!(store.purge().unwrap() > 0);
    let a = store.loops("a.ts").unwrap();
    assert!(a.iter().any(|l| l.id == pinned && l.pinned));
    assert!(
        a.iter()
            .all(|l| l.pinned || recent.lines().any(|h| h == l.head_sha))
    );
    assert_eq!(store.loops("b.ts").unwrap().len(), 7);
    assert_eq!(
        store
            .index
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

#[test]
fn marker_only_working_difference_does_not_disable_retention() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["config", "core.autocrlf", "false"]);
    fs::write(root.join("a.ts"), "base\n").unwrap();
    git(root, &["add", "a.ts"]);
    git(root, &["commit", "-qm", "base"]);
    for n in 0..5 {
        let text = format!("//> change {n}\nbody {n}\n");
        fs::write(root.join("a.ts"), &text).unwrap();
        open(root).capture(&[]).unwrap();
        git(root, &["add", "a.ts"]);
        let repo = Repo::discover(&["-C".into(), root.as_os_str().to_owned()]).unwrap();
        assert_eq!(paradox_core::commit::strip_staged(&repo).unwrap(), 1);
        git(root, &["commit", "-qm", &format!("change {n}")]);
        assert_eq!(fs::read_to_string(root.join("a.ts")).unwrap(), text);
    }
    let mut store = open(root);
    assert!(store.purge().unwrap() > 0);
    assert_eq!(store.loops("a.ts").unwrap().len(), 2);
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
