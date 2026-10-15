#![cfg(unix)]

use paradox_core::{repo::Repo, store::LoopStore};
use std::fs;
use std::os::unix::fs::PermissionsExt;
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
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
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
fn marker_only_bytes_with_an_uncommitted_mode_change_keep_old_loops() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "core.filemode", "true"]);
    git(root, &["config", "core.autocrlf", "false"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("a.ts"), "base\n").unwrap();
    fs::set_permissions(root.join("a.ts"), fs::Permissions::from_mode(0o644)).unwrap();
    git(root, &["add", "a.ts"]);
    git(root, &["commit", "-qm", "base"]);
    for n in 0..5 {
        fs::write(root.join("a.ts"), format!("//> change {n}\nbody {n}\n")).unwrap();
        open(root).capture(&[]).unwrap();
        git(root, &["add", "a.ts"]);
        let repo = Repo::discover(&["-C".into(), root.as_os_str().to_owned()]).unwrap();
        assert_eq!(paradox_core::commit::strip_staged(&repo).unwrap(), 1);
        git(root, &["commit", "-qm", &format!("change {n}")]);
    }
    let before = open(root).loops("a.ts").unwrap();
    assert_eq!(before.len(), 5);
    fs::set_permissions(root.join("a.ts"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(git(root, &["diff", "--summary", "--", "a.ts"]).contains("100644 => 100755"));
    let mut store = open(root);
    assert_eq!(store.purge().unwrap(), 0);
    let retained = store.loops("a.ts").unwrap();
    assert_eq!(retained.len(), before.len());
    assert!(
        before
            .iter()
            .all(|old| retained.iter().any(|new| old.id == new.id))
    );
}
