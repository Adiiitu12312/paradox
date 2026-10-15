use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn command(executable: &str, root: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(executable);
    command
        .current_dir(root)
        .args(arguments)
        .stdin(Stdio::null());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-global"))
        .env("GIT_AUTHOR_NAME", "Paradox Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Paradox Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn px(root: &Path, arguments: &[&str]) -> Output {
    command(env!("CARGO_BIN_EXE_paradox"), root, arguments)
}

fn git(root: &Path, arguments: &[&str]) {
    let output = command("git", root, arguments);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn help_and_invalid_options_work_without_repository_or_sidecar() {
    let root = tempfile::tempdir().unwrap();
    let help = px(root.path(), &["timeline", "--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Choose a recent or uncommitted file"));
    let invalid = px(root.path(), &["timeline", "--unknown"]);
    assert_eq!(invalid.status.code(), Some(2));
    let outside = px(root.path(), &["timeline"]);
    assert_eq!(outside.status.code(), Some(1));
    assert!(!root.path().join(".paradox").exists());
    assert!(!root.path().join(".git").exists());
}

#[test]
fn empty_picker_and_nonterminal_failure_do_not_open_or_create_store() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q", "--initial-branch=main"]);
    git(root.path(), &["config", "commit.gpgsign", "false"]);
    git(root.path(), &["config", "core.hooksPath", "no-hooks"]);
    let exclude_before = fs::read(root.path().join(".git/info/exclude")).unwrap();
    // Raw untracked noise is not eligible, including in an unborn repository.
    fs::create_dir(root.path().join("noise")).unwrap();
    fs::write(root.path().join("noise/untracked.ts"), "noise\n").unwrap();
    let empty = px(root.path(), &["timeline"]);
    assert!(empty.status.success());
    assert!(
        String::from_utf8_lossy(&empty.stdout).contains("No recent or uncommitted tracked files.")
    );
    assert!(!root.path().join(".paradox").exists());
    fs::write(root.path().join("a.ts"), "base\n").unwrap();
    git(root.path(), &["add", "a.ts"]);
    git(root.path(), &["commit", "-qm", "base"]);
    let clean = px(root.path(), &["timeline"]);
    assert!(clean.status.success());
    assert!(!root.path().join(".paradox").exists());
    fs::write(root.path().join("a.ts"), "//> pending\nchanged\n").unwrap();
    let no_terminal = px(root.path(), &["timeline"]);
    assert!(
        !no_terminal.status.success(),
        "a picker requires an interactive terminal"
    );
    assert!(
        !root.path().join(".paradox").exists(),
        "waiting or failing to prompt must not open a store"
    );
    assert_eq!(
        fs::read(root.path().join(".git/info/exclude")).unwrap(),
        exclude_before
    );
}
