use paradox_core::{repo::Repo, store::LoopStore};
use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn command(root: &Path, binary: &str) -> Command {
    let mut command = Command::new(binary);
    command
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

fn open(root: &Path) -> LoopStore {
    LoopStore::open(Repo::discover(&["-C".into(), root.as_os_str().to_owned()]).unwrap()).unwrap()
}

#[test]
fn failing_original_post_commit_does_not_prevent_retention_after_successful_git_commits() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    git(root, &["init", "-q", "--initial-branch=main"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["config", "core.autocrlf", "false"]);
    fs::write(root.join("a.ts"), "base\n").unwrap();
    git(root, &["add", "a.ts"]);
    git(root, &["commit", "-qm", "base"]);
    let hook = root.join(".git/hooks/post-commit");
    fs::write(&hook, "#!/bin/sh\nprintf 'called\\n' >> user-post-ran\nexit 37\n").unwrap();
    paradox_core::setup::executable(&hook).unwrap();
    ok(command(root, env!("CARGO_BIN_EXE_paradox"))
        .arg("init")
        .stdin(Stdio::null())
        .output()
        .unwrap());

    let mut pinned = 0;
    for n in 1..=6 {
        let working = format!("//> change {n}\nbody {n}\n");
        fs::write(root.join("a.ts"), &working).unwrap();
        // Raw Git proves that its installed post-commit chain performs purge;
        // no wrapper-level purge is available to hide an early hook exit.
        git(root, &["add", "a.ts"]);
        git(root, &["commit", "-qm", &format!("change {n}")]);
        assert_eq!(git(root, &["show", "HEAD:a.ts"]), format!("body {n}\n"));
        assert_eq!(fs::read(root.join("a.ts")).unwrap(), working.as_bytes());
        if n == 1 {
            let store = open(root);
            pinned = store.loops("a.ts").unwrap()[0].id;
            store.pin_loop(pinned, true).unwrap();
        }
    }
    assert_eq!(
        fs::read_to_string(root.join("user-post-ran")).unwrap(),
        "called\n".repeat(6)
    );
    let store = open(root);
    let retained = store.loops("a.ts").unwrap();
    assert_eq!(retained.len(), 3);
    assert!(retained.iter().any(|entry| entry.id == pinned && entry.pinned));
    let mut recent_messages: Vec<_> = retained
        .iter()
        .filter(|entry| !entry.pinned)
        .map(|entry| entry.message.as_str())
        .collect();
    recent_messages.sort_unstable();
    assert_eq!(recent_messages, ["change 5", "change 6"]);
}
