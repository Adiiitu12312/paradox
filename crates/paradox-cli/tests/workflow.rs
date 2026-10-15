use paradox_core::{repo::Repo, store::LoopStore};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn command(root: &Path, binary: &str) -> Command {
    let mut c = Command::new(binary);
    c.current_dir(root)
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
    c
}
fn px(root: &Path, args: &[&str]) -> Output {
    command(root, env!("CARGO_BIN_EXE_paradox"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
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
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q", "--initial-branch=main"]);
    git(root.path(), &["config", "commit.gpgsign", "false"]);
    git(root.path(), &["config", "core.autocrlf", "false"]);
    root
}
fn store(root: &Path) -> LoopStore {
    LoopStore::open(Repo::discover(&["-C".into(), root.as_os_str().to_owned()]).unwrap()).unwrap()
}
fn script(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    paradox_core::setup::executable(path).unwrap();
}

#[test]
fn init_merges_jsonc_is_idempotent_and_uninstalls_only_owned_entries() {
    let root = fixture();
    let root = root.path();
    fs::create_dir(root.join(".vscode")).unwrap();
    fs::create_dir(root.join(".claude")).unwrap();
    fs::write(root.join(".vscode/settings.json"), "{\n// keep this comment\n\"editor.tabSize\": 3,\n\"emeraldwalk.runonsave\": {\"commands\":[{\"cmd\":\"existing command\"},],},\n}\n").unwrap();
    fs::write(root.join(".claude/settings.json"), "{\"permissions\": {\"allow\":[\"Read\",]}, \"hooks\": {\"PostToolUse\":[{\"matcher\":\"Read\",\"hooks\":[]},],},}\n").unwrap();
    ok(px(root, &["init"]));
    let settings = fs::read_to_string(root.join(".vscode/settings.json")).unwrap();
    assert!(
        settings.contains("// keep this comment")
            && settings.contains("existing command")
            && settings.contains("\"isAsync\": true")
    );
    assert!(settings.contains("${file}"));
    let claude = fs::read_to_string(root.join(".claude/settings.json")).unwrap();
    assert!(
        claude.contains("Edit|Write|MultiEdit")
            && claude.contains("paradox loop --hook claude")
            && claude.contains("\"Read\"")
    );
    ok(px(root, &["init"]));
    assert_eq!(
        fs::read_to_string(root.join(".vscode/settings.json")).unwrap(),
        settings
    );
    ok(px(root, &["init", "--uninstall"]));
    let settings = fs::read_to_string(root.join(".vscode/settings.json")).unwrap();
    assert!(
        settings.contains("existing command")
            && settings.contains("// keep this comment")
            && !settings.contains("${file}")
    );
    let claude = fs::read_to_string(root.join(".claude/settings.json")).unwrap();
    assert!(claude.contains("\"Read\"") && !claude.contains("paradox loop"));
    assert!(!root.join(".git/hooks/pre-commit").exists());
    ok(px(root, &["init", "--uninstall"]));
}

#[test]
fn print_commands_do_not_initialize_a_repository_or_install_anything() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let rule = ok(px(
        root,
        &[
            "init",
            "--print-agent-rule",
            "--guide-path",
            "docs/Agent Guide.md",
        ],
    ));
    for text in [
        "docs/Agent Guide.md",
        "paradox commit",
        "paradox track",
        "paradox watch --background",
        "line 2",
        "/*>",
        "<!-->",
    ] {
        assert!(rule.contains(text), "{rule}");
    }
    for editor in ["neovim", "jetbrains", "sublime"] {
        assert!(!ok(px(root, &["init", "--print-editor-config", editor])).is_empty());
    }
    assert_eq!(fs::read_dir(root).unwrap().count(), 0);
    ok(px(root, &["init"]));
    assert!(root.join(".git").is_dir());
    assert!(root.join(".paradox").is_dir());
    assert_eq!(
        store(root)
            .index
            .query_row("SELECT COUNT(*) FROM loops", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn raw_git_commit_chains_hooks_and_strips_every_marker_without_touching_working_bytes() {
    let root = fixture();
    let root = root.path();
    git(root, &["config", "core.hooksPath", "custom-hooks"]);
    fs::create_dir(root.join("custom-hooks")).unwrap();
    script(
        &root.join("custom-hooks/pre-commit"),
        "#!/bin/sh\nprintf 'first\\n' >> hook-order\n",
    );
    let original = fs::read(root.join("custom-hooks/pre-commit")).unwrap();
    ok(px(root, &["init"]));
    let forms = [
        ("a.ts", "//> msg\r\nbody\r\n"),
        ("b.py", "#> msg\nbody"),
        ("c.sql", "-- > msg\nbody\n"),
        ("d.ini", ";> msg\nbody\n"),
        ("e.tex", "%> msg\nbody\n"),
        ("f.css", "/*> msg */\r\nbody\r\n"),
        ("g.html", "<!--> msg -->\nbody\n"),
        ("h.sh", "#!/bin/sh\n#> msg\nbody\n"),
        ("i.php", "<?php\r\n//> msg\r\nbody\r\n"),
        ("j.sql", "--> msg\nbody\n"),
    ];
    for (name, bytes) in forms {
        fs::write(root.join(name), bytes).unwrap();
    }
    fs::write(root.join("no.json"), "//> invalid JSON comment\n{}").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "markers"]);
    assert_eq!(
        fs::read_to_string(root.join("hook-order")).unwrap(),
        "first\n"
    );
    for (name, bytes) in forms {
        assert_eq!(fs::read(root.join(name)).unwrap(), bytes.as_bytes());
        let range = paradox_core::message::marker_range(Path::new(name), bytes.as_bytes()).unwrap();
        let expected = format!("{}{}", &bytes[..range.start], &bytes[range.end..]);
        assert_eq!(git(root, &["show", &format!("HEAD:{name}")]), expected);
    }
    assert_eq!(
        git(root, &["show", "HEAD:no.json"]),
        "//> invalid JSON comment\n{}"
    );
    assert!(!git(root, &["ls-tree", "-r", "--name-only", "HEAD"]).contains(".paradox/"));
    ok(px(root, &["init", "--uninstall"]));
    assert_eq!(
        fs::read(root.join("custom-hooks/pre-commit")).unwrap(),
        original
    );
}

#[test]
fn wrapper_partial_and_file_first_commits_preserve_unrelated_staging_and_skip_user_hooks_on_no_verify()
 {
    let root = fixture();
    let root = root.path();
    fs::write(root.join("a.ts"), "base a\n").unwrap();
    fs::write(root.join("b.ts"), "base b\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "base"]);
    fs::write(root.join("a.ts"), "//> first\nfirst\n").unwrap();
    fs::write(root.join("b.ts"), "other staged\n").unwrap();
    git(root, &["add", "b.ts"]);
    ok(px(root, &["commit", "a.ts", "-m", "one file"]));
    assert_eq!(git(root, &["show", "HEAD:a.ts"]), "first\n");
    assert_eq!(git(root, &["show", "HEAD:b.ts"]), "base b\n");
    assert!(git(root, &["diff", "--cached", "--name-only"]).contains("b.ts"));
    fs::write(root.join("a.ts"), "/*> next */\nnext\n").unwrap();
    ok(px(root, &["a.ts", "commit", "-m", "file first"]));
    assert_eq!(git(root, &["show", "HEAD:a.ts"]), "next\n");
    script(&root.join(".git/hooks/pre-commit"), "#!/bin/sh\nexit 37\n");
    script(&root.join(".git/hooks/commit-msg"), "#!/bin/sh\nexit 38\n");
    fs::write(root.join("a.ts"), "//> bypass user hook\nfinal\n").unwrap();
    assert!(!px(root, &["commit", "-m", "blocked"]).status.success());
    ok(px(root, &["commit", "--no-verify", "-m", "all files"]));
    assert_eq!(git(root, &["show", "HEAD:a.ts"]), "final\n");
    assert_eq!(git(root, &["show", "HEAD:b.ts"]), "other staged\n");
    assert_eq!(
        fs::read_to_string(root.join("a.ts")).unwrap(),
        "//> bypass user hook\nfinal\n"
    );
}

#[test]
fn staged_content_is_stripped_even_when_working_content_differs_and_mode_is_preserved() {
    let root = fixture();
    let root = root.path();
    ok(px(root, &["init"]));
    fs::write(root.join("a.ts"), "//> staged\nstaged bytes\n").unwrap();
    git(root, &["add", "a.ts"]);
    git(root, &["update-index", "--chmod=+x", "a.ts"]);
    fs::write(root.join("a.ts"), "//> unstaged\nworking bytes\n").unwrap();
    git(root, &["commit", "-qm", "partial stage"]);
    assert_eq!(git(root, &["show", "HEAD:a.ts"]), "staged bytes\n");
    assert!(git(root, &["ls-tree", "HEAD", "a.ts"]).starts_with("100755"));
    assert_eq!(
        fs::read_to_string(root.join("a.ts")).unwrap(),
        "//> unstaged\nworking bytes\n"
    );
}

#[test]
fn ai_hook_payload_creates_a_save_loop_and_deduplicates_repeated_edits() {
    let root = fixture();
    let root = root.path();
    fs::write(root.join("a.ts"), "base\n").unwrap();
    git(root, &["add", "a.ts"]);
    git(root, &["commit", "-qm", "base"]);
    ok(px(root, &["init"]));
    fs::write(root.join("a.ts"), "//> AI edit\nchanged\n").unwrap();
    let input =
        serde_json::json!({"tool_name":"Edit","tool_input":{"file_path":root.join("a.ts")}})
            .to_string();
    for _ in 0..2 {
        let mut child = command(root, env!("CARGO_BIN_EXE_paradox"))
            .args(["loop", "--hook", "claude"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        ok(child.wait_with_output().unwrap());
    }
    let s = store(root);
    let loops = s.loops("a.ts").unwrap();
    assert_eq!(loops.len(), 1);
    assert_eq!(loops[0].message, "AI edit");
    assert_eq!(loops[0].kind, "save");
}
