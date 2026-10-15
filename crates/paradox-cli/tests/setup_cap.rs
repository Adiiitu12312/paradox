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
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    command
}

fn ok(output: Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git(root: &Path, args: &[&str]) {
    ok(command(root, "git").args(args).output().unwrap());
}

fn open(root: &Path) -> LoopStore {
    LoopStore::open(Repo::discover(&["-C".into(), root.as_os_str().to_owned()]).unwrap()).unwrap()
}

fn noisy_text(mut seed: u32) -> Vec<u8> {
    let size = 100_000 + seed as usize;
    (0..size)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            33 + (seed % 94) as u8
        })
        .collect()
}

#[test]
fn init_refuses_manifest_over_cap_before_changing_existing_hooks_or_settings() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    git(root, &["init", "-q", "--initial-branch=main"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["config", "core.autocrlf", "false"]);
    fs::write(root.join("a.ts"), "base\n").unwrap();
    git(root, &["add", "a.ts"]);
    git(root, &["commit", "-qm", "base"]);
    let hook = root.join(".git/hooks/pre-commit");
    let original_hook = b"#!/bin/sh\n# original hook must survive refused setup\nexit 0\n";
    fs::write(&hook, original_hook).unwrap();
    paradox_core::setup::executable(&hook).unwrap();
    fs::create_dir(root.join(".vscode")).unwrap();
    let settings = root.join(".vscode/settings.json");
    let original_settings = b"{\n// preserve these exact bytes\n\"editor.tabSize\": 3,\n}\n";
    fs::write(&settings, original_settings).unwrap();

    let mut store = open(root);
    let mut ids = Vec::new();
    for seed in 1..=3 {
        fs::write(root.join("a.ts"), noisy_text(seed)).unwrap();
        store.capture(&[root.join("a.ts")]).unwrap();
        let id = store.loops("a.ts").unwrap()[0].id;
        store.pin_loop(id, true).unwrap();
        ids.push(id);
    }
    store.gc().unwrap();
    let current_bytes = store.bytes().unwrap();
    assert!(current_bytes > 131_072);
    let cap = current_bytes + 512;
    let store_root = store.repo.store.clone();
    let config = format!("store_cap_bytes = {cap}\n");
    assert!(config.len() < 512);
    fs::write(store_root.join("config.toml"), config).unwrap();
    assert!(store.bytes().unwrap() <= cap);
    drop(store);

    let output = command(root, env!("CARGO_BIN_EXE_paradox"))
        .arg("init")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "setup unexpectedly succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(fs::read(&hook).unwrap(), original_hook);
    assert_eq!(fs::read(&settings).unwrap(), original_settings);
    assert!(!root.join(".git/hooks/pre-commit.paradox-original").exists());
    assert!(!store_root.join("setup-hooks.json").exists());
    let store = open(root);
    assert!(store.bytes().unwrap() <= cap);
    let retained = store.loops("a.ts").unwrap();
    assert_eq!(retained.len(), ids.len());
    assert!(
        ids.iter()
            .all(|id| retained.iter().any(|entry| entry.id == *id && entry.pinned))
    );
}
