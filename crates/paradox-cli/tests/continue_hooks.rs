use paradox_core::{
    repo::Repo,
    ui_actions::{self, Action, Cancellation, Operation, ResolveRequest},
};
use serde_json::json;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn git_output(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    command
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-global"))
        .env("GIT_EDITOR", "true")
        .output()
        .unwrap()
}
fn git(root: &Path, args: &[&str]) -> String {
    let output = git_output(root, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn repo(root: &Path) -> Repo {
    Repo::discover(&["-C".into(), root.as_os_str().to_owned()]).unwrap()
}
fn fixture(operation: Operation) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let p = root.path();
    git(p, &["init", "-q", "--initial-branch=main"]);
    for (key, value) in [
        ("user.name", "Fixture"),
        ("user.email", "fixture@example.invalid"),
        ("commit.gpgsign", "false"),
        ("core.autocrlf", "false"),
        ("core.hooksPath", ".git/user hooks"),
    ] {
        git(p, &["config", key, value]);
    }
    fs::write(p.join("a.ts"), "base\n").unwrap();
    git(p, &["add", "a.ts"]);
    git(p, &["commit", "-qm", "base"]);
    git(p, &["checkout", "-qb", "topic"]);
    fs::write(p.join("a.ts"), "theirs\n").unwrap();
    git(p, &["commit", "-qam", "topic"]);
    git(p, &["checkout", "-q", "main"]);
    fs::write(p.join("a.ts"), "ours\n").unwrap();
    git(p, &["commit", "-qam", "main"]);
    let output = match operation {
        Operation::Merge => git_output(p, &["merge", "--no-edit", "topic"]),
        Operation::Rebase => {
            git(p, &["checkout", "-q", "topic"]);
            git_output(p, &["rebase", "main"])
        }
        _ => unreachable!(),
    };
    assert!(!output.status.success());
    let conflict = ui_actions::conflict(repo(p), "a.ts").unwrap();
    let resolution: ResolveRequest = serde_json::from_value(json!({"path":"a.ts", "token":conflict["token"], "choice":"manual", "result":"<!--> resolved by agent -->\nresolved bytes\n", "mark_resolved":true})).unwrap();
    ui_actions::resolve_conflict(repo(p), resolution).unwrap();
    root
}
fn hook(root: &Path, event: &str, body: &str) -> Vec<u8> {
    let directory = root.join(".git/user hooks");
    fs::create_dir_all(&directory).unwrap();
    let bytes = format!(
        "#!/bin/sh\nprintf '%s\\n' '{}' >> hook-calls\n{body}\n",
        event
    )
    .into_bytes();
    let path = directory.join(event);
    fs::write(&path, &bytes).unwrap();
    paradox_core::setup::executable(&path).unwrap();
    bytes
}
fn run(root: &Path, operation: Operation) -> anyhow::Result<serde_json::Value> {
    let mut events = Vec::new();
    let result = ui_actions::execute_with_executable(
        repo(root),
        Action::Continue { operation },
        None,
        Cancellation::new(),
        Path::new(env!("CARGO_BIN_EXE_paradox")),
        |event| events.push(event),
    );
    if result.is_err() {
        eprintln!("native Continue events: {events:?}");
    }
    result
}

#[test]
fn continue_hooks_cannot_reintroduce_markers_and_keep_native_sequencer_behavior() {
    let root = fixture(Operation::Merge);
    let p = root.path();
    let working = fs::read(p.join("a.ts")).unwrap();
    let mut originals = Vec::new();
    for event in ["pre-commit", "prepare-commit-msg", "commit-msg"] {
        let body = "oid=$(printf '/*> hook marker */\\nresolved bytes\\n' | git hash-object -w --stdin) || exit $?\ngit update-index --cacheinfo 100644 \"$oid\" a.ts || exit $?";
        originals.push((event, hook(p, event, body)));
    }
    run(p, Operation::Merge).unwrap();
    assert_eq!(git(p, &["show", "HEAD:a.ts"]), "resolved bytes\n");
    assert_eq!(git(p, &["show", ":a.ts"]), "resolved bytes\n");
    assert_eq!(fs::read(p.join("a.ts")).unwrap(), working);
    let calls = fs::read_to_string(p.join("hook-calls")).unwrap();
    assert!(
        calls.lines().any(|event| event == "prepare-commit-msg"),
        "{calls}"
    );
    assert!(calls.lines().any(|event| event == "pre-commit"), "{calls}");
    assert!(calls.lines().any(|event| event == "commit-msg"), "{calls}");
    for (event, bytes) in originals {
        assert_eq!(
            fs::read(p.join(".git/user hooks").join(event)).unwrap(),
            bytes
        );
    }
    assert_eq!(
        git(p, &["config", "core.hooksPath"]).trim(),
        ".git/user hooks"
    );
}

#[test]
fn rebase_marker_preflight_refuses_without_changing_files_index_or_head() {
    let root = fixture(Operation::Rebase);
    let p = root.path();
    let head = git(p, &["rev-parse", "HEAD"]);
    let working = fs::read(p.join("a.ts")).unwrap();
    let index = fs::read(p.join(".git/index")).unwrap();
    let original = hook(p, "prepare-commit-msg", ":");
    let error = run(p, Operation::Rebase).unwrap_err();
    assert!(
        error.to_string().contains("Remove the message line"),
        "{error:#}"
    );
    assert_eq!(git(p, &["rev-parse", "HEAD"]), head);
    assert_eq!(fs::read(p.join("a.ts")).unwrap(), working);
    assert_eq!(fs::read(p.join(".git/index")).unwrap(), index);
    assert!(!p.join("hook-calls").exists());
    assert_eq!(
        fs::read(p.join(".git/user hooks/prepare-commit-msg")).unwrap(),
        original
    );
}

#[test]
fn marker_free_rebase_preserves_native_prepare_hook_and_its_rejection() {
    for reject in [false, true] {
        let root = fixture(Operation::Rebase);
        let p = root.path();
        fs::write(p.join("a.ts"), "resolved bytes\n").unwrap();
        git(p, &["add", "a.ts"]);
        let head = git(p, &["rev-parse", "HEAD"]);
        let original = hook(
            p,
            "prepare-commit-msg",
            if reject { "exit 39" } else { ":" },
        );
        let result = run(p, Operation::Rebase);
        if reject {
            assert!(result.is_err());
            assert_eq!(git(p, &["rev-parse", "HEAD"]), head);
        } else {
            result.unwrap();
            assert_eq!(git(p, &["show", "HEAD:a.ts"]), "resolved bytes\n");
            assert!(!p.join(".git/rebase-merge").exists());
        }
        assert_eq!(
            fs::read_to_string(p.join("a.ts")).unwrap(),
            "resolved bytes\n"
        );
        assert_eq!(
            fs::read_to_string(p.join("hook-calls")).unwrap(),
            "prepare-commit-msg\n"
        );
        assert_eq!(
            fs::read(p.join(".git/user hooks/prepare-commit-msg")).unwrap(),
            original
        );
    }
}

#[test]
fn rebase_prepare_hook_index_reinjection_refuses_before_committing() {
    let root = fixture(Operation::Rebase);
    let p = root.path();
    fs::write(p.join("a.ts"), "resolved bytes\n").unwrap();
    git(p, &["add", "a.ts"]);
    let head = git(p, &["rev-parse", "HEAD"]);
    hook(
        p,
        "prepare-commit-msg",
        "oid=$(printf '/*> injected */\\nresolved bytes\\n' | git hash-object -w --stdin) || exit $?\ngit update-index --cacheinfo 100644 \"$oid\" a.ts || exit $?",
    );
    assert!(run(p, Operation::Rebase).is_err());
    assert_eq!(git(p, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read_to_string(p.join("a.ts")).unwrap(),
        "resolved bytes\n"
    );
    // The user's hook changed the index. Refusal must not claim otherwise.
    assert_eq!(
        git(p, &["show", ":a.ts"]),
        "/*> injected */\nresolved bytes\n"
    );
    assert!(p.join(".git/rebase-merge").is_dir());
}

#[test]
fn continue_still_rejects_a_failing_user_commit_message_hook() {
    let root = fixture(Operation::Merge);
    let p = root.path();
    let head = git(p, &["rev-parse", "HEAD"]);
    let working = fs::read(p.join("a.ts")).unwrap();
    let original = hook(p, "commit-msg", "exit 37");
    assert!(run(p, Operation::Merge).is_err());
    assert_eq!(git(p, &["rev-parse", "HEAD"]), head);
    assert!(p.join(".git/MERGE_HEAD").is_file());
    assert_eq!(fs::read(p.join("a.ts")).unwrap(), working);
    assert_eq!(
        fs::read(p.join(".git/user hooks/commit-msg")).unwrap(),
        original
    );
    assert_eq!(
        fs::read_to_string(p.join("hook-calls")).unwrap(),
        "commit-msg\n"
    );
}

#[test]
fn ordinary_commit_message_hook_cannot_reintroduce_an_index_marker() {
    let root = fixture(Operation::Merge);
    let p = root.path();
    hook(
        p,
        "commit-msg",
        "oid=$(printf '#> hook marker\\nresolved bytes\\n' | git hash-object -w --stdin) || exit $?\ngit update-index --cacheinfo 100644 \"$oid\" a.ts || exit $?",
    );
    let working = fs::read(p.join("a.ts")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_paradox"))
        .args(["-C"])
        .arg(p)
        .args(["commit", "-m", "resolved"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(git(p, &["show", "HEAD:a.ts"]), "resolved bytes\n");
    assert_eq!(fs::read(p.join("a.ts")).unwrap(), working);
}
