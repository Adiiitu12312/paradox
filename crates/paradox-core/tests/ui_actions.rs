use paradox_core::{
    repo::Repo,
    store::LoopStore,
    ui_actions::{self, Action, Cancellation, Operation, ResetMode, ResolveRequest},
    ui_git::ActionError,
};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    process::{Command, Output},
};
struct Fixture(tempfile::TempDir);
impl Fixture {
    fn new() -> Self {
        let f = Self(tempfile::tempdir().unwrap());
        f.git(&["init", "-q", "--initial-branch=main"]);
        for (k, v) in [
            ("user.name", "Fixture"),
            ("user.email", "fixture@example.invalid"),
            ("core.autocrlf", "false"),
            ("core.hooksPath", ".git/no-hooks"),
            ("commit.gpgsign", "false"),
        ] {
            f.git(&["config", k, v]);
        }
        f
    }
    fn output(&self, args: &[&str]) -> Output {
        let mut command = Command::new("git");
        command.current_dir(self.0.path()).args(args);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.0.path().join("absent"))
            .output()
            .unwrap()
    }
    fn git(&self, args: &[&str]) -> String {
        let output = self.output(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn repo(&self) -> Repo {
        Repo::discover(&[OsString::from("-C"), self.0.path().as_os_str().to_owned()]).unwrap()
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        fs::write(self.0.path().join(path), bytes).unwrap();
    }
    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", message]);
    }
    fn initial(&self) {
        self.write("a.ts", b"base\n");
        self.commit("initial");
    }
    fn run(&self, action: Action) -> Value {
        let plan = ui_actions::preview(self.repo(), action.clone()).unwrap();
        // Core's libtest executable is not the CLI. Its hook adapter executes
        // this crate's real stripper; CLI integration tests cover real hooks.
        let hooks = tempfile::tempdir().unwrap();
        let adapter = hooks.path().join("paradox-test-hook");
        let executable = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        fs::write(&adapter, format!("#!/bin/sh\n[ \"$3\" = post-commit ] && exit 0\nexport PARADOX_CORE_HOOK_TEST=1\nexec {} --ignored --exact mandatory_hook_test_entry --nocapture\n", paradox_core::setup::shell_quote(&executable))).unwrap();
        paradox_core::setup::executable(&adapter).unwrap();
        ui_actions::execute_with_executable(
            self.repo(),
            action,
            Some(&plan),
            Cancellation::new(),
            &adapter,
            |_| {},
        )
        .unwrap()
    }
}

#[test]
#[ignore = "invoked only by the temporary Git hook adapter"]
fn mandatory_hook_test_entry() {
    if std::env::var_os("PARADOX_CORE_HOOK_TEST").is_some() {
        let cwd = std::env::current_dir().unwrap();
        let repo = Repo::discover(&["-C".into(), cwd.into_os_string()]).unwrap();
        paradox_core::commit::strip_staged(&repo).unwrap();
    }
}

#[test]
fn branch_tag_state_and_noninteractive_history_actions_are_native_git() {
    let f = Fixture::new();
    f.initial();
    f.run(Action::BranchCreate {
        name: "topic".into(),
        start: None,
    });
    f.run(Action::BranchSwitch {
        name: "topic".into(),
    });
    f.write("topic.ts", b"topic\n");
    f.commit("topic commit");
    let topic = f.git(&["rev-parse", "HEAD"]).trim().to_owned();
    f.run(Action::TagCreate {
        name: "v1".into(),
        target: None,
        message: Some("annotated".into()),
    });
    let state = ui_actions::state(f.repo()).unwrap();
    assert!(
        state["branches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "topic" && row["current"] == true)
    );
    assert_eq!(state["tags"][0]["sha"], topic);
    f.run(Action::BranchSwitch {
        name: "main".into(),
    });
    f.run(Action::Merge {
        revision: "topic".into(),
    });
    assert_eq!(f.git(&["rev-parse", "HEAD"]).trim(), topic);
    f.run(Action::BranchRename {
        name: "topic".into(),
        new_name: "renamed".into(),
    });
    f.run(Action::BranchDelete {
        name: "renamed".into(),
        force: false,
    });
    f.run(Action::TagDelete { name: "v1".into() });
    f.run(Action::Revert { revision: topic });
    assert!(!f.0.path().join("topic.ts").exists());
    let error = ui_actions::preview(
        f.repo(),
        Action::Rebase {
            onto: "main".into(),
            interactive: true,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("paradox rebase -i"));
}

#[test]
fn stash_create_apply_pop_and_drop_use_stable_object_ids() {
    let f = Fixture::new();
    f.initial();
    f.write("a.ts", b"changed\n");
    f.write("new.ts", b"new\n");
    f.run(Action::StashCreate {
        message: Some("literal -- stash".into()),
        include_untracked: true,
    });
    assert_eq!(fs::read(f.0.path().join("a.ts")).unwrap(), b"base\n");
    assert!(!f.0.path().join("new.ts").exists());
    let state = ui_actions::state(f.repo()).unwrap();
    let oid = state["stashes"][0]["oid"].as_str().unwrap().to_owned();
    f.run(Action::StashApply {
        oid: oid.clone(),
        pop: false,
    });
    assert_eq!(fs::read(f.0.path().join("a.ts")).unwrap(), b"changed\n");
    f.git(&["reset", "--hard", "HEAD"]);
    fs::remove_file(f.0.path().join("new.ts")).unwrap();
    f.run(Action::StashApply { oid, pop: true });
    assert!(
        ui_actions::state(f.repo()).unwrap()["stashes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    f.run(Action::StashCreate {
        message: None,
        include_untracked: true,
    });
    let state = ui_actions::state(f.repo()).unwrap();
    f.run(Action::StashDrop {
        oid: state["stashes"][0]["oid"].as_str().unwrap().into(),
    });
    assert!(
        ui_actions::state(f.repo()).unwrap()["stashes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn destructive_plan_stales_and_safety_preserves_distinct_staged_and_working_bytes() {
    let f = Fixture::new();
    f.initial();
    f.write("a.ts", b"staged\n");
    f.git(&["add", "a.ts"]);
    f.write("a.ts", b"working\n");
    let action = Action::Reset {
        target: "HEAD".into(),
        mode: ResetMode::Hard,
    };
    let plan = ui_actions::preview(f.repo(), action.clone()).unwrap();
    assert_eq!(plan.safety.len(), 2);
    f.write("a.ts", b"new working\n");
    let error = ui_actions::execute(
        f.repo(),
        action.clone(),
        Some(&plan),
        Cancellation::new(),
        |_| {},
    )
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<ActionError>().unwrap().code,
        "stale_plan"
    );
    assert_eq!(fs::read(f.0.path().join("a.ts")).unwrap(), b"new working\n");
    let result = f.run(action);
    assert_eq!(fs::read(f.0.path().join("a.ts")).unwrap(), b"base\n");
    let store = LoopStore::open(f.repo()).unwrap();
    for expected in [b"staged\n".as_slice(), b"new working\n".as_slice()] {
        assert!(
            result["safety_loops"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| {
                    let id = row["id"].as_i64().unwrap();
                    let version = store.resolve("a.ts", &id.to_string()).unwrap();
                    store.version_bytes("a.ts", &version).unwrap() == expected
                })
        );
    }
}

#[test]
fn skipped_protection_is_explicit_and_discard_has_a_recoverable_safety_loop() {
    let f = Fixture::new();
    f.initial();
    f.write("a.ts", b"discard me\n");
    let action = Action::Discard {
        paths: vec!["a.ts".into()],
    };
    let result = f.run(action);
    assert_eq!(fs::read(f.0.path().join("a.ts")).unwrap(), b"base\n");
    assert!(!result["safety_loops"].as_array().unwrap().is_empty());
    f.write(".env", b"SECRET=do-not-store\n");
    f.git(&["add", ".env"]);
    f.write("binary.bin", b"\0binary");
    f.git(&["add", "binary.bin"]);
    let plan = ui_actions::preview(
        f.repo(),
        Action::Reset {
            target: "HEAD".into(),
            mode: ResetMode::Hard,
        },
    )
    .unwrap();
    assert!(
        plan.skipped
            .iter()
            .any(|entry| entry.path == ".env" && entry.reason.as_ref().unwrap().contains("secret"))
    );
    assert!(plan.skipped.iter().any(
        |entry| entry.path == "binary.bin" && entry.reason.as_ref().unwrap().contains("binary")
    ));
}

#[test]
fn conflict_manual_resolution_is_stale_checked_protected_and_marked_resolved() {
    let f = Fixture::new();
    f.initial();
    f.git(&["checkout", "-qb", "topic"]);
    f.write("a.ts", b"theirs\n");
    f.commit("theirs");
    f.git(&["checkout", "-q", "main"]);
    f.write("a.ts", b"ours\n");
    f.commit("ours");
    assert!(!f.output(&["merge", "--no-edit", "topic"]).status.success());
    let conflict = ui_actions::conflict(f.repo(), "a.ts").unwrap();
    assert_eq!(conflict["ours"]["text"], "ours\n");
    assert_eq!(conflict["theirs"]["text"], "theirs\n");
    let request = |token: String| {
        serde_json::from_value::<ResolveRequest>(json!({"path":"a.ts","token":token,"choice":"manual","result":"resolved\n","mark_resolved":true})).unwrap()
    };
    let old = conflict["token"].as_str().unwrap().to_owned();
    f.write("a.ts", b"external edit\n");
    assert!(ui_actions::resolve_conflict(f.repo(), request(old)).is_err());
    let conflict = ui_actions::conflict(f.repo(), "a.ts").unwrap();
    let result = ui_actions::resolve_conflict(
        f.repo(),
        request(conflict["token"].as_str().unwrap().into()),
    )
    .unwrap();
    assert_eq!(result["resolved"], true);
    assert!(f.git(&["ls-files", "--unmerged"]).is_empty());
    assert_eq!(f.git(&["show", ":a.ts"]), "resolved\n");
    f.run(Action::Continue {
        operation: Operation::Merge,
    });
    assert!(
        f.git(&["rev-list", "--parents", "-1", "HEAD"])
            .split_whitespace()
            .count()
            == 3
    );
}

#[test]
fn blame_and_history_follow_native_commits_and_renames() {
    let f = Fixture::new();
    f.initial();
    f.git(&["mv", "a.ts", "renamed é.ts"]);
    f.commit("rename");
    let history = ui_actions::history(f.repo(), "renamed é.ts", None).unwrap();
    assert_eq!(history["commits"].as_array().unwrap().len(), 2);
    let blame = ui_actions::blame(f.repo(), "renamed é.ts", None).unwrap();
    assert_eq!(blame["lines"][0]["text"], "base\n");
    assert_eq!(blame["lines"][0]["summary"], "initial");
    assert!(ui_actions::blame(f.repo(), "../outside", None).is_err());
}

#[test]
fn local_bare_remote_streams_progress_and_rejects_an_outdated_explicit_lease() {
    let f = Fixture::new();
    f.initial();
    let bare = tempfile::tempdir().unwrap();
    let status = Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(bare.path())
        .status()
        .unwrap();
    assert!(status.success());
    f.git(&["remote", "add", "origin", bare.path().to_str().unwrap()]);
    let mut progress = Vec::new();
    ui_actions::execute(
        f.repo(),
        Action::Push {
            remote: "origin".into(),
            branch: "main".into(),
            force_with_lease: false,
        },
        None,
        Cancellation::new(),
        |event| progress.push(serde_json::to_value(event).unwrap()),
    )
    .unwrap();
    assert!(progress.iter().any(|event| event["type"] == "command"));
    assert!(progress.iter().any(|event| event["type"] == "exit"));
    f.run(Action::Fetch {
        remote: "origin".into(),
        prune: true,
    });
    let force = Action::Push {
        remote: "origin".into(),
        branch: "main".into(),
        force_with_lease: true,
    };
    let plan = ui_actions::preview(f.repo(), force.clone()).unwrap();
    let observed = f
        .git(&["rev-parse", "refs/remotes/origin/main"])
        .trim()
        .to_owned();
    assert!(
        plan.losses
            .iter()
            .any(|loss| loss.description.contains(&observed))
    );
    assert!(
        plan.reflog_hints
            .iter()
            .any(|hint| hint.contains(&observed))
    );
    // Advance only the remote; the local preview remains valid but its exact
    // observed-tip lease must reject the race rather than overwrite new history.
    f.write("a.ts", b"second\n");
    f.commit("second");
    let second = f.git(&["rev-parse", "HEAD"]);
    f.git(&["push", "origin", "main"]);
    f.git(&["reset", "--hard", "HEAD~1"]);
    let fresh = ui_actions::preview(f.repo(), force.clone()).unwrap();
    let expected = plan
        .commands
        .iter()
        .flat_map(|row| row.stdout.lines())
        .find(|line| line.len() == 40)
        .unwrap_or_default();
    assert!(!expected.is_empty());
    // Restore the old tracking observation without altering the remote.
    f.git(&["update-ref", "refs/remotes/origin/main", expected]);
    let stale_remote = ui_actions::preview(f.repo(), force.clone()).unwrap();
    let error = ui_actions::execute(
        f.repo(),
        force,
        Some(&stale_remote),
        Cancellation::new(),
        |_| {},
    )
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<ActionError>().unwrap().code,
        "git_failed"
    );
    let remote = Command::new("git")
        .arg("--git-dir")
        .arg(bare.path())
        .args(["rev-parse", "refs/heads/main"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(remote.stdout).unwrap(), second);
    drop(fresh);
}

#[test]
fn clean_rebase_and_cherry_pick_reuse_native_git_history() {
    let f = Fixture::new();
    f.initial();
    f.run(Action::BranchCreate {
        name: "topic".into(),
        start: None,
    });
    f.run(Action::BranchSwitch {
        name: "topic".into(),
    });
    f.write("topic.ts", b"topic\n");
    f.commit("topic");
    f.run(Action::BranchSwitch {
        name: "main".into(),
    });
    f.write("main.ts", b"main\n");
    f.commit("main advanced");
    let main = f.git(&["rev-parse", "HEAD"]).trim().to_owned();
    f.run(Action::BranchSwitch {
        name: "topic".into(),
    });
    f.run(Action::Rebase {
        onto: "main".into(),
        interactive: false,
    });
    assert_eq!(f.git(&["rev-parse", "HEAD~1"]).trim(), main);
    let topic = f.git(&["rev-parse", "HEAD"]).trim().to_owned();
    f.run(Action::BranchSwitch {
        name: "main".into(),
    });
    f.run(Action::CherryPick { revision: topic });
    assert_eq!(f.git(&["show", "HEAD:topic.ts"]), "topic\n");
    assert_eq!(f.git(&["show", "HEAD:main.ts"]), "main\n");
}

#[test]
fn discard_restores_both_index_and_working_bytes_and_handles_unborn_new_files() {
    let f = Fixture::new();
    f.initial();
    f.write("a.ts", b"staged\n");
    f.git(&["add", "a.ts"]);
    f.write("a.ts", b"working\n");
    f.run(Action::Discard {
        paths: vec!["a.ts".into()],
    });
    assert_eq!(f.git(&["show", ":a.ts"]), "base\n");
    assert_eq!(fs::read(f.0.path().join("a.ts")).unwrap(), b"base\n");
    assert!(f.git(&["status", "--porcelain"]).is_empty());
    let unborn = Fixture::new();
    unborn.write("new.ts", b"staged\n");
    unborn.git(&["add", "new.ts"]);
    unborn.write("new.ts", b"working\n");
    let result = unborn.run(Action::Discard {
        paths: vec!["new.ts".into()],
    });
    assert!(!unborn.0.path().join("new.ts").exists());
    assert!(unborn.git(&["ls-files"]).is_empty());
    assert_eq!(result["safety_loops"].as_array().unwrap().len(), 2);
}

#[test]
fn safety_operation_never_unpins_an_existing_user_pin_with_identical_content() {
    let f = Fixture::new();
    f.initial();
    f.write("a.ts", b"preserve pinned content\n");
    let mut store = LoopStore::open(f.repo()).unwrap();
    store.capture(&[f.0.path().join("a.ts")]).unwrap();
    let original = store.loops("a.ts").unwrap()[0].id;
    store.pin_loop(original, true).unwrap();
    drop(store);
    f.run(Action::Reset {
        target: "HEAD".into(),
        mode: ResetMode::Hard,
    });
    let store = LoopStore::open(f.repo()).unwrap();
    let pinned = store
        .loops("a.ts")
        .unwrap()
        .into_iter()
        .find(|row| row.id == original)
        .unwrap();
    assert!(pinned.pinned);
    let version = store.resolve("a.ts", &original.to_string()).unwrap();
    assert_eq!(
        store.version_bytes("a.ts", &version).unwrap(),
        b"preserve pinned content\n"
    );
}

#[test]
fn loss_previews_name_exact_branch_tag_and_reset_objects_and_bound_commit_lists() {
    use std::io::Write;
    use std::process::Stdio;
    let f = Fixture::new();
    f.initial();
    let base = f.git(&["rev-parse", "HEAD"]).trim().to_owned();
    let mut import = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(f.0.path())
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut input = import.stdin.take().unwrap();
        for index in 0..60 {
            let subject = format!("unique loss {index}");
            write!(input,"commit refs/heads/topic\ncommitter Fixture <fixture@example.invalid> {} +0000\ndata {}\n{}\n{}\n",1_700_000_000+index,subject.len(),subject,if index==0{format!("from {base}\n")}else{String::new()}).unwrap();
        }
        writeln!(input, "done").unwrap();
    }
    let output = import.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tip = f.git(&["rev-parse", "topic"]).trim().to_owned();
    let branch = ui_actions::preview(
        f.repo(),
        Action::BranchDelete {
            name: "topic".into(),
            force: true,
        },
    )
    .unwrap();
    assert!(
        branch
            .losses
            .iter()
            .any(|loss| loss.description.contains(&tip))
    );
    assert!(
        branch
            .losses
            .iter()
            .any(|loss| loss.description.contains("60 commits are not merged"))
    );
    assert!(
        branch
            .losses
            .iter()
            .any(|loss| loss.description.contains("10 additional commits omitted"))
    );
    assert_eq!(
        branch
            .losses
            .iter()
            .filter(|loss| loss.description.starts_with("Commit:"))
            .count(),
        50
    );
    assert!(branch.reflog_hints.iter().any(|hint| hint.contains(&tip)));
    f.git(&["tag", "-a", "exact-tag", "-m", "annotation", "topic"]);
    let tag_object = f
        .git(&["rev-parse", "refs/tags/exact-tag"])
        .trim()
        .to_owned();
    let tag = ui_actions::preview(
        f.repo(),
        Action::TagDelete {
            name: "exact-tag".into(),
        },
    )
    .unwrap();
    assert!(
        tag.losses
            .iter()
            .any(|loss| loss.description.contains(&tag_object))
    );
    assert!(
        tag.reflog_hints
            .iter()
            .any(|hint| hint.contains(&tag_object))
    );
    f.git(&["checkout", "-q", "topic"]);
    let reset = ui_actions::preview(
        f.repo(),
        Action::Reset {
            target: base.clone(),
            mode: ResetMode::Soft,
        },
    )
    .unwrap();
    assert!(
        reset
            .losses
            .iter()
            .any(|loss| loss.description.contains(&tip) && loss.description.contains(&base))
    );
    assert!(reset.safety.is_empty());
    assert!(reset.reflog_hints.iter().any(|hint| hint.contains(&tip)));
}

#[test]
fn a_long_lived_repo_discards_to_current_branch_head_and_labels_safety_correctly() {
    let f = Fixture::new();
    f.initial();
    let session_repo = f.repo();
    let startup = session_repo.head.clone();
    f.git(&["checkout", "-qb", "new-head"]);
    f.write("a.ts", b"current branch baseline\n");
    f.commit("current branch commit");
    let current = f.git(&["rev-parse", "HEAD"]).trim().to_owned();
    assert_ne!(startup, current);
    f.write("a.ts", b"working edit after switch\n");
    let action = Action::Discard {
        paths: vec!["a.ts".into()],
    };
    let plan = ui_actions::preview(session_repo.clone(), action.clone()).unwrap();
    let result = ui_actions::execute(
        session_repo,
        action,
        Some(&plan),
        Cancellation::new(),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        fs::read(f.0.path().join("a.ts")).unwrap(),
        b"current branch baseline\n"
    );
    assert_eq!(f.git(&["show", ":a.ts"]), "current branch baseline\n");
    let store = LoopStore::open(f.repo()).unwrap();
    let rows = store.loops("a.ts").unwrap();
    for safety in result["safety_loops"].as_array().unwrap() {
        let id = safety["id"].as_i64().unwrap();
        let row = rows.iter().find(|row| row.id == id).unwrap();
        assert_eq!(row.head_sha, current);
    }
}

#[test]
fn continue_merge_strips_a_closed_ai_marker_from_the_resolved_index_only() {
    let f = Fixture::new();
    f.initial();
    f.git(&["checkout", "-qb", "topic"]);
    f.write("a.ts", b"theirs\n");
    f.commit("topic");
    f.git(&["checkout", "-q", "main"]);
    f.write("a.ts", b"ours\n");
    f.commit("main");
    assert!(!f.output(&["merge", "--no-edit", "topic"]).status.success());
    let conflict = ui_actions::conflict(f.repo(), "a.ts").unwrap();
    let working = "/*> combine both branches */\nresolved bytes\n";
    let request=serde_json::from_value::<ResolveRequest>(json!({"path":"a.ts","token":conflict["token"],"choice":"manual","result":working,"mark_resolved":true})).unwrap();
    ui_actions::resolve_conflict(f.repo(), request).unwrap();
    assert_eq!(f.git(&["show", ":a.ts"]), working);
    f.run(Action::Continue {
        operation: Operation::Merge,
    });
    assert_eq!(f.git(&["show", "HEAD:a.ts"]), "resolved bytes\n");
    assert_eq!(f.git(&["show", ":a.ts"]), "resolved bytes\n");
    assert_eq!(
        fs::read_to_string(f.0.path().join("a.ts")).unwrap(),
        working
    );
}
