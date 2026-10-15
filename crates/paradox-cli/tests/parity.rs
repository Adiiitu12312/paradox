//! Every invocation of Paradox is confined to OS temporary repositories.
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

fn configured(exe: impl AsRef<std::ffi::OsStr>, root: &Path) -> Command {
    let mut c = Command::new(exe);
    c.current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-global-config"))
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Paradox Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Paradox Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("LC_ALL", "C")
        .env("TERM", "dumb");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
    ] {
        c.env_remove(name);
    }
    c
}

fn git(root: &Path, args: &[&str]) -> Output {
    configured("git", root).args(args).output().unwrap()
}

fn ok(root: &Path, args: &[&str]) {
    let o = git(root, args);
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn fixture() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    assert!(!dir.path().starts_with(env!("CARGO_MANIFEST_DIR")));
    let r = dir.path();
    ok(r, &["init", "-q", "--initial-branch=main"]);
    ok(r, &["config", "core.autocrlf", "false"]);
    ok(r, &["config", "commit.gpgsign", "false"]);
    ok(r, &["config", "tag.gpgsign", "false"]);
    ok(r, &["config", "core.hooksPath", ".git/no-hooks"]);
    fs::write(r.join("file.txt"), "first\n").unwrap();
    ok(r, &["add", "."]);
    ok(r, &["commit", "-qm", "first"]);
    fs::write(r.join("file.txt"), "first\nsecond\n").unwrap();
    ok(r, &["add", "."]);
    ok(r, &["commit", "-qm", "second"]);
    dir
}

fn assert_same(git: Output, paradox: Output, args: &[&str]) {
    assert_eq!(git.status.code(), paradox.status.code(), "exit: {args:?}");
    assert_eq!(git.stdout, paradox.stdout, "stdout: {args:?}");
    assert_eq!(git.stderr, paradox.stderr, "stderr: {args:?}");
}

#[test]
fn forty_plus_read_only_commands_match_byte_for_byte() {
    let repo = fixture();
    fs::write(repo.path().join("file.txt"), "first\nchanged\n").unwrap();
    fs::write(repo.path().join("new file ü.txt"), "untracked\n").unwrap();
    let cases: &[&[&str]] = &[
        &["status"],
        &["status", "--short"],
        &["status", "--porcelain=v2", "-z"],
        &["status", "--ignored"],
        &["log", "--graph"],
        &["log", "--graph", "--oneline"],
        &["log", "--format=%H%x00%P%x00%s"],
        &["log", "-1", "--stat"],
        &["diff"],
        &["diff", "--numstat", "-z"],
        &["diff", "--cached"],
        &["diff", "--exit-code"],
        &["diff", "--color=always"],
        &["show", "HEAD"],
        &["show", "HEAD:file.txt"],
        &["show", "--format=raw", "--no-patch"],
        &["branch"],
        &["branch", "-avv"],
        &["tag", "--list"],
        &["remote", "-v"],
        &["stash", "list"],
        &["reflog", "--format=%H %gs"],
        &["rev-parse", "HEAD"],
        &["rev-list", "--count", "HEAD"],
        &["ls-files", "-z"],
        &["ls-tree", "HEAD"],
        &["for-each-ref", "--format=%(refname)"],
        &["blame", "--porcelain", "HEAD", "--", "file.txt"],
        &["grep", "first"],
        &["grep", "absent"],
        &["submodule", "status"],
        &["config", "--local", "--get", "core.autocrlf"],
        &["config", "--get", "absent.value"],
        &["count-objects", "-v"],
        &["fsck", "--no-reflogs"],
        &["check-ref-format", "refs/heads/topic"],
        &["--no-pager", "log", "--oneline"],
        &["-c", "color.ui=always", "status", "--short"],
        &["-c", "user.name=x", "log", "-1", "--oneline"],
        &["-C", ".", "status"],
        &["-C", ".", "-C", ".", "diff"],
        &["--git-dir=.git", "--work-tree=.", "status"],
        &["--no-optional-locks", "status"],
        &["--version"],
        &["--exec-path"],
        &["does-not-exist"],
        &["--invalid-global-option", "loop"],
        &["-C"],
        &["diff", "--", "loop"],
        &["show", "commit"],
        &["log", "--bad-option"],
    ];
    for args in cases {
        let expected = git(repo.path(), args);
        let actual = configured(env!("CARGO_BIN_EXE_paradox"), repo.path())
            .args(*args)
            .output()
            .unwrap();
        assert_same(expected, actual, args);
    }
    assert!(!repo.path().join(".paradox").exists());
    println!(
        "{} read-only/error Git combinations matched exactly",
        cases.len()
    );
}

#[test]
fn mutating_commands_match_in_identical_repositories() {
    let direct = fixture();
    let wrapped = fixture();
    let cases: &[&[&str]] = &[
        &["branch", "topic"],
        &["switch", "topic"],
        &["tag", "v1"],
        &["config", "test.value", "unicode ü and spaces"],
        &["config", "--get", "test.value"],
        &["rebase", "main"],
        &["rebase", "-i", "HEAD~1"],
        &[
            "cherry-pick",
            "--allow-empty",
            "--keep-redundant-commits",
            "HEAD",
        ],
        &["revert", "--no-edit", "HEAD"],
        &["switch", "main"],
        &["merge", "--ff-only", "topic"],
        &["tag", "-d", "v1"],
        &["branch", "-d", "topic"],
        &["gc", "--quiet"],
    ];
    for args in cases {
        assert_same(
            git(direct.path(), args),
            configured(env!("CARGO_BIN_EXE_paradox"), wrapped.path())
                .args(*args)
                .output()
                .unwrap(),
            args,
        );
    }
    for root in [direct.path(), wrapped.path()] {
        fs::write(root.join("file.txt"), "stash me\n").unwrap();
    }
    for args in [
        &["stash", "push", "-m", "test"][..],
        &["stash", "pop"],
        &["restore", "file.txt"],
    ] {
        assert_same(
            git(direct.path(), args),
            configured(env!("CARGO_BIN_EXE_paradox"), wrapped.path())
                .args(args)
                .output()
                .unwrap(),
            args,
        );
    }
    assert_eq!(
        git(direct.path(), &["rev-parse", "HEAD"]).stdout,
        git(wrapped.path(), &["rev-parse", "HEAD"]).stdout
    );
}

#[test]
fn forced_git_keeps_globals_and_supports_commit_and_init() {
    let dir = tempfile::tempdir().unwrap();
    let o = configured(env!("CARGO_BIN_EXE_paradox"), dir.path())
        .args(["-C", ".", "git", "init", "-q", "--initial-branch=main"])
        .output()
        .unwrap();
    assert!(o.status.success());
    fs::write(dir.path().join("file.txt"), "hello\n").unwrap();
    ok(dir.path(), &["add", "."]);
    let o = configured(env!("CARGO_BIN_EXE_paradox"), dir.path())
        .args(["-c", "user.name=x", "git", "commit", "-qm", "hello"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!dir.path().join(".paradox").exists());
}

#[test]
fn stdin_and_special_arguments_reach_git_unmodified() {
    let repo = fixture();
    let input = b"spaces, quotes \" ' & % ! \\ and unicode \xc3\xbc\n";
    let run = |exe: &str| {
        let mut c = configured(exe, repo.path())
            .args(["hash-object", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin.take().unwrap().write_all(input).unwrap();
        c.wait_with_output().unwrap()
    };
    assert_same(
        run("git"),
        run(env!("CARGO_BIN_EXE_paradox")),
        &["hash-object", "--stdin"],
    );
    let args = [
        "-c",
        "alias.argv=!printf '%s\\n'",
        "argv",
        "a b",
        "a\"b",
        "ü",
        "",
        "trailing\\",
    ];
    assert_same(
        git(repo.path(), &args),
        configured(env!("CARGO_BIN_EXE_paradox"), repo.path())
            .args(args)
            .output()
            .unwrap(),
        &args,
    );
}

#[test]
fn collision_guard_checks_git_help_and_installed_commands() {
    let dir = tempfile::tempdir().unwrap();
    let list = git(dir.path(), &["--list-cmds=main,others,alias"]);
    let help = git(dir.path(), &["help", "-a"]);
    assert!(list.status.success() && help.status.success());
    let listed = String::from_utf8(list.stdout).unwrap();
    let helped = String::from_utf8(help.stdout).unwrap();
    for owned in paradox_cli::dispatch::OWNED
        .iter()
        .filter(|c| !["commit", "init"].contains(c))
    {
        assert!(
            !listed.lines().any(|l| l.trim() == *owned),
            "Git collision: {owned}"
        );
        assert!(
            !helped
                .lines()
                .any(|l| l.starts_with("   ") && l.split_whitespace().next() == Some(*owned)),
            "Git help collision: {owned}"
        );
    }
}

#[test]
fn missing_git_is_clear_and_ui_help_does_not_modify_repo() {
    let dir = tempfile::tempdir().unwrap();
    let o = configured(env!("CARGO_BIN_EXE_paradox"), dir.path())
        .env("PATH", "")
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(127));
    assert!(String::from_utf8_lossy(&o.stderr).contains("Git executable not found"));
    for owned in paradox_cli::dispatch::OWNED
        .iter()
        .filter(|c| ["ui"].contains(c))
    {
        let o = configured(env!("CARGO_BIN_EXE_paradox"), dir.path())
            .arg(owned)
            .arg("--help")
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(0), "{owned}");
    }
    assert!(!dir.path().join(".git").exists());
    assert!(!dir.path().join(".paradox").exists());
}

#[test]
fn worktrees_and_submodules_work_with_passthrough() {
    let repo = fixture();
    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().join("work tree ü");
    let o = configured(env!("CARGO_BIN_EXE_paradox"), repo.path())
        .args([
            OsString::from("worktree"),
            "add".into(),
            "-b".into(),
            "extra".into(),
            worktree.as_os_str().into(),
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_same(
        git(&worktree, &["status"]),
        configured(env!("CARGO_BIN_EXE_paradox"), &worktree)
            .arg("status")
            .output()
            .unwrap(),
        &["status"],
    );
    let child = fixture();
    let o = configured(env!("CARGO_BIN_EXE_paradox"), repo.path())
        .args([
            OsString::from("-c"),
            "protocol.file.allow=always".into(),
            "submodule".into(),
            "add".into(),
            child.path().as_os_str().into(),
            "module".into(),
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_same(
        git(repo.path(), &["submodule", "status"]),
        configured(env!("CARGO_BIN_EXE_paradox"), repo.path())
            .args(["submodule", "status"])
            .output()
            .unwrap(),
        &["submodule", "status"],
    );
}
