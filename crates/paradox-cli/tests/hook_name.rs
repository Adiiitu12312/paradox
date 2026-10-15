use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn command(root: &Path, binary: &str) -> Command {
    let mut command = Command::new(binary);
    command.current_dir(root)
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
    assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}

fn git(root: &Path, args: &[&str]) -> String {
    ok(command(root, "git").args(args).output().unwrap())
}

fn px(root: &Path, args: &[&str]) {
    ok(command(root, env!("CARGO_BIN_EXE_paradox")).args(args).stdin(Stdio::null()).output().unwrap());
}

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::Builder::new().prefix("paradox hook name ").tempdir().unwrap();
    let root = temp.path();
    git(root, &["init", "-q", "--initial-branch=main"]);
    git(root, &["config", "core.autocrlf", "false"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("a.ts"), "base\n").unwrap();
    git(root, &["add", "a.ts"]);
    git(root, &["commit", "-qm", "base"]);
    temp
}

fn executable(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    paradox_core::setup::executable(path).unwrap();
}

#[test]
fn installed_shell_hooks_keep_original_name_and_directory_and_uninstall_exact_bytes() {
    for interpreter in ["#!/bin/sh", "#!/usr/bin/env bash"] {
        let temp = fixture();
        let root = temp.path();
        let hooks = root.join("hooks with spaces");
        fs::create_dir(&hooks).unwrap();
        git(root, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
        fs::write(hooks.join("original-helper"), "helper exists\n").unwrap();
        let original = format!("{interpreter}\n# dispatch like a hook manager\nname=$(basename \"$0\")\n[ \"$name\" = pre-commit ] || exit 41\ndir=$(dirname \"$0\")\n[ -f \"$dir/original-helper\" ] || exit 42\nprintf '%s\\n' \"$name\" >> original-hook-names\n");
        let hook = hooks.join("pre-commit");
        executable(&hook, original.as_bytes());
        px(root, &["init"]);
        let backup = hooks.join("pre-commit.paradox-original");
        assert_eq!(fs::read(&backup).unwrap(), original.as_bytes());
        let working = "//> keep marker local\nchanged\n";
        fs::write(root.join("a.ts"), working).unwrap();
        git(root, &["add", "a.ts"]);
        git(root, &["commit", "-qm", "original hook name"]);
        assert_eq!(fs::read_to_string(root.join("original-hook-names")).unwrap(), "pre-commit\n");
        assert_eq!(git(root, &["show", "HEAD:a.ts"]), "changed\n");
        assert_eq!(fs::read(root.join("a.ts")).unwrap(), working.as_bytes());
        assert_eq!(fs::read(&backup).unwrap(), original.as_bytes());
        px(root, &["init", "--uninstall"]);
        assert_eq!(fs::read(&hook).unwrap(), original.as_bytes());
        assert!(!backup.exists());
    }
}

#[test]
fn renamed_shell_delegation_preserves_push_arguments_stdin_and_failure_status() {
    let temp = fixture();
    let root = temp.path();
    let remote = tempfile::Builder::new().prefix("paradox remote space ").tempdir().unwrap();
    git(root, &["init", "--bare", "-q", remote.path().to_str().unwrap()]);
    git(root, &["remote", "add", "origin", remote.path().to_str().unwrap()]);
    let hook = root.join(".git/hooks/pre-push");
    let backup = root.join(".git/hooks/pre-push.paradox-original");
    let original = b"#!/bin/sh\n[ \"$(basename \"$0\")\" = pre-push ] || exit 41\nprintf '%s\\n' \"$1\" \"$2\" > push-args\ncat > push-stdin\nexit 37\n";
    executable(&backup, original);
    let wrapper = paradox_core::setup::hook_script(Path::new(env!("CARGO_BIN_EXE_paradox")), Some(&backup), None).unwrap();
    executable(&hook, wrapper.as_bytes());
    let output = command(root, "git").args(["push", "origin", "main"]).output().unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(&backup).unwrap(), original);
    let args = fs::read_to_string(root.join("push-args")).unwrap();
    let mut lines = args.lines();
    assert_eq!(lines.next(), Some("origin"));
    let actual_remote = lines.next().unwrap().replace('\\', "/");
    assert_eq!(actual_remote, remote.path().to_str().unwrap().replace('\\', "/"));
    assert_eq!(lines.next(), None);
    let input = fs::read_to_string(root.join("push-stdin")).unwrap();
    let fields: Vec<_> = input.split_ascii_whitespace().collect();
    assert_eq!(fields.len(), 4);
    assert_eq!(fields[0], "refs/heads/main");
    assert_eq!(fields[1], git(root, &["rev-parse", "HEAD"]).trim());
    assert_eq!(fields[2], "refs/heads/main");
    assert!(fields[3].bytes().all(|byte| byte == b'0'));
    assert!(git(root, &["--git-dir", remote.path().to_str().unwrap(), "for-each-ref", "--format=%(refname)"]).is_empty());
    let status = command(root, "git")
        .args(["hook", "run", "pre-push", "--", "origin", "remote"])
        .stdin(Stdio::null())
        .output()
        .unwrap()
        .status;
    assert_eq!(status.code(), Some(37));
}
