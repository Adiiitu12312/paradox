use std::fs;
use std::io::Write;
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

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    git(root, &["init", "-q", "--initial-branch=main"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["config", "core.autocrlf", "false"]);
    for name in ["selected file.ts", "a.ts", "b.ts", "c.ts"] {
        fs::write(root.join(name), format!("base {name}\n")).unwrap();
    }
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "base"]);
    temp
}

#[test]
fn nul_pathspec_stdin_is_available_to_both_staging_and_partial_commit() {
    let variants: &[&[&str]] = &[
        &["commit", "--pathspec-from-file=-", "--pathspec-file-nul", "-m", "stdin selection"],
        &["commit", "--pathspec-from-file", "-", "--pathspec-file-nul", "-m", "stdin selection"],
    ];
    for args in variants {
        let temp = fixture();
        let root = temp.path();
        let selected = "/*> selected only */\r\nselected change\r\n";
        let unrelated = "//> keep staged marker\nunrelated staged change\n";
        fs::write(root.join("selected file.ts"), selected).unwrap();
        fs::write(root.join("b.ts"), unrelated).unwrap();
        git(root, &["add", "b.ts"]);
        let mut child = command(root, env!("CARGO_BIN_EXE_paradox"))
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Closing this pipe makes a second attempt to consume stdin see EOF.
        child.stdin.take().unwrap().write_all(b"selected file.ts\0").unwrap();
        ok(child.wait_with_output().unwrap());
        assert_eq!(git(root, &["show", "HEAD:selected file.ts"]), "selected change\r\n");
        assert_eq!(git(root, &["show", "HEAD:b.ts"]), "base b.ts\n");
        assert_eq!(git(root, &["show", ":b.ts"]), unrelated);
        assert_eq!(git(root, &["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"]), "selected file.ts\n");
        assert_eq!(fs::read(root.join("selected file.ts")).unwrap(), selected.as_bytes());
        assert_eq!(fs::read(root.join("b.ts")).unwrap(), unrelated.as_bytes());
    }
}

#[test]
fn file_first_commit_distinguishes_a_literal_dash_message_from_the_actual_separator() {
    let temp = fixture();
    let root = temp.path();
    let first = "//> first file\nfirst change\n";
    let last = "<!--> last file -->\nlast change\n";
    let unrelated = "unrelated staged change\n";
    fs::write(root.join("a.ts"), first).unwrap();
    fs::write(root.join("c.ts"), last).unwrap();
    fs::write(root.join("b.ts"), unrelated).unwrap();
    git(root, &["add", "b.ts"]);
    ok(command(root, env!("CARGO_BIN_EXE_paradox"))
        .args(["a.ts", "commit", "-m", "--", "--", "c.ts"])
        .stdin(Stdio::null())
        .output()
        .unwrap());
    assert_eq!(git(root, &["log", "-1", "--format=%s"]).trim_end(), "--");
    assert_eq!(git(root, &["show", "HEAD:a.ts"]), "first change\n");
    assert_eq!(git(root, &["show", "HEAD:c.ts"]), "last change\n");
    assert_eq!(git(root, &["show", "HEAD:b.ts"]), "base b.ts\n");
    assert_eq!(git(root, &["show", ":b.ts"]), unrelated);
    assert_eq!(fs::read(root.join("a.ts")).unwrap(), first.as_bytes());
    assert_eq!(fs::read(root.join("c.ts")).unwrap(), last.as_bytes());
}
