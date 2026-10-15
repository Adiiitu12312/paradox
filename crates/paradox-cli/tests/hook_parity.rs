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

fn px(root: &Path, args: &[&str]) -> Output {
    command(root, env!("CARGO_BIN_EXE_paradox"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    git(root, &["init", "-q", "--initial-branch=main"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["config", "core.autocrlf", "false"]);
    fs::write(root.join("a.ts"), "base\n").unwrap();
    git(root, &["add", "a.ts"]);
    git(root, &["commit", "-qm", "base"]);
    temp
}

fn script(path: &Path, bytes: &str) {
    fs::write(path, bytes).unwrap();
    paradox_core::setup::executable(path).unwrap();
}

#[test]
fn combined_and_abbreviated_no_verify_skip_user_hooks_but_always_strip_markers() {
    let variants: &[&[&str]] = &[
        &["commit", "-nm", "combined"],
        &["commit", "-qnm", "quiet combined"],
        &["commit", "--no-veri", "-m", "abbreviated"],
    ];
    for args in variants {
        let temp = fixture();
        let root = temp.path();
        script(
            &root.join(".git/hooks/pre-commit"),
            "#!/bin/sh\nprintf pre > user-hook-ran\nexit 37\n",
        );
        script(
            &root.join(".git/hooks/commit-msg"),
            "#!/bin/sh\nprintf message > user-hook-ran\nexit 38\n",
        );
        let working = "/*> keep local message */\r\nchanged\r\n";
        fs::write(root.join("a.ts"), working).unwrap();
        ok(px(root, args));
        assert_eq!(git(root, &["show", "HEAD:a.ts"]), "changed\r\n");
        assert_eq!(fs::read(root.join("a.ts")).unwrap(), working.as_bytes());
        assert!(!root.join("user-hook-ran").exists());
    }
}

#[test]
fn verify_after_no_verify_restores_user_hook_rejection() {
    let temp = fixture();
    let root = temp.path();
    script(
        &root.join(".git/hooks/pre-commit"),
        "#!/bin/sh\nprintf rejected > user-hook-ran\nexit 37\n",
    );
    fs::write(root.join("a.ts"), "//> do not leak\nchanged\n").unwrap();
    let before = git(root, &["rev-parse", "HEAD"]);
    let output = px(root, &["commit", "--no-verify", "--verify", "-m", "blocked"]);
    assert!(!output.status.success());
    assert_eq!(git(root, &["rev-parse", "HEAD"]), before);
    assert_eq!(fs::read_to_string(root.join("user-hook-ran")).unwrap(), "rejected");
}

#[test]
fn shared_worktree_hook_installation_is_idempotent_and_last_uninstall_restores_original() {
    let temp = fixture();
    let root = temp.path();
    let second = tempfile::tempdir().unwrap();
    git(root, &["worktree", "add", "-q", "-b", "second", second.path().to_str().unwrap()]);
    let hook = root.join(".git/hooks/pre-commit");
    let original = "#!/bin/sh\n# preserve original bytes\nprintf original > original-hook-ran\n";
    script(&hook, original);
    ok(px(root, &["init"]));
    let managed = fs::read(&hook).unwrap();
    ok(px(second.path(), &["init"]));
    ok(px(second.path(), &["init"]));
    assert_eq!(fs::read(&hook).unwrap(), managed);
    assert_eq!(fs::read(root.join(".git/hooks/pre-commit.paradox-original")).unwrap(), original.as_bytes());
    ok(px(second.path(), &["init", "--uninstall"]));
    assert_eq!(fs::read(&hook).unwrap(), managed);
    fs::write(root.join("a.ts"), "//> shared hook\nchanged\n").unwrap();
    git(root, &["add", "a.ts"]);
    git(root, &["commit", "-qm", "shared hook still installed"]);
    assert_eq!(git(root, &["show", "HEAD:a.ts"]), "changed\n");
    assert_eq!(fs::read_to_string(root.join("original-hook-ran")).unwrap(), "original");
    ok(px(root, &["init", "--uninstall"]));
    assert_eq!(fs::read(&hook).unwrap(), original.as_bytes());
    assert!(!root.join(".git/hooks/pre-commit.paradox-original").exists());
}

#[cfg(unix)]
#[test]
fn non_executable_hooks_remain_ignored_by_wrapper_and_installed_hooks() {
    use std::os::unix::fs::PermissionsExt;
    for installed in [false, true] {
        let temp = fixture();
        let root = temp.path();
        for event in ["pre-commit", "post-commit", "commit-msg", "prepare-commit-msg"] {
            let path = root.join(".git/hooks").join(event);
            fs::write(&path, "#!/bin/sh\nprintf unexpected > ignored-hook-ran\nexit 37\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        if installed {
            ok(px(root, &["init"]));
        }
        fs::write(root.join("a.ts"), "//> wrapper\nwrapper change\n").unwrap();
        ok(px(root, &["commit", "-m", "ignore nonexec"]));
        assert_eq!(git(root, &["show", "HEAD:a.ts"]), "wrapper change\n");
        assert!(!root.join("ignored-hook-ran").exists());
        if installed {
            fs::write(root.join("a.ts"), "//> raw git\nraw change\n").unwrap();
            git(root, &["add", "a.ts"]);
            git(root, &["commit", "-qm", "raw ignores nonexec"]);
            assert_eq!(git(root, &["show", "HEAD:a.ts"]), "raw change\n");
            assert!(!root.join("ignored-hook-ran").exists());
        }
    }
}

#[cfg(unix)]
#[test]
fn executable_symlink_hooks_preserve_prepare_and_commit_message_callbacks() {
    use std::os::unix::fs::symlink;
    let temp = fixture();
    let root = temp.path();
    for (event, body) in [
        ("prepare-commit-msg", "#!/bin/sh\nprintf 'prepared\\n' >> hook-order\nprintf ' prepared' >> \"$1\"\n"),
        ("commit-msg", "#!/bin/sh\nprintf 'checked\\n' >> hook-order\ngrep -q prepared \"$1\" || exit 38\n"),
    ] {
        let target = root.join(format!("{event}-target"));
        script(&target, body);
        symlink(&target, root.join(".git/hooks").join(event)).unwrap();
    }
    fs::write(root.join("a.ts"), "//> symlink hook\nchanged\n").unwrap();
    ok(px(root, &["commit", "a.ts", "-m", "symlink hooks"]));
    assert_eq!(fs::read_to_string(root.join("hook-order")).unwrap(), "prepared\nchecked\n");
    assert!(git(root, &["log", "-1", "--format=%B"]).contains("prepared"));
    assert_eq!(git(root, &["show", "HEAD:a.ts"]), "changed\n");
}
