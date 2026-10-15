use std::{fs, path::Path, process::Command};
fn command(root: &Path, exe: &str) -> Command {
    let mut c = Command::new(exe);
    c.current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("no-global"))
        .env("GIT_AUTHOR_NAME", "Paradox Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Paradox Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid");
    c
}
#[test]
fn configured_editor_command_records_a_save_and_exits() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for args in [
        vec!["init", "-q"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["config", "core.autocrlf", "false"],
    ] {
        assert!(command(root, "git").args(args).status().unwrap().success());
    }
    fs::write(root.join("file with spaces.ts"), "base\n").unwrap();
    assert!(
        command(root, "git")
            .args(["add", "-A"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        command(root, "git")
            .args(["commit", "-qm", "base"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        command(root, env!("CARGO_BIN_EXE_paradox"))
            .arg("init")
            .status()
            .unwrap()
            .success()
    );
    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join(".vscode/settings.json")).unwrap()).unwrap();
    let entry = &settings["emeraldwalk.runonsave"]["commands"][0];
    assert_eq!(entry["isAsync"], true);
    let configured = entry["cmd"].as_str().unwrap().replace(
        "${file}",
        root.join("file with spaces.ts").to_str().unwrap(),
    );
    fs::write(
        root.join("file with spaces.ts"),
        "//> editor saved\nchanged\n",
    )
    .unwrap();
    let mut paths = vec![
        Path::new(env!("CARGO_BIN_EXE_paradox"))
            .parent()
            .unwrap()
            .to_path_buf(),
    ];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    assert!(
        command(root, "git")
            .env("PATH", std::env::join_paths(paths).unwrap())
            .args([
                "-c",
                &format!("alias.paradox-save=!{configured}"),
                "paradox-save"
            ])
            .status()
            .unwrap()
            .success()
    );
    let repo =
        paradox_core::repo::Repo::discover(&["-C".into(), root.as_os_str().to_owned()]).unwrap();
    let s = paradox_core::store::LoopStore::open(repo).unwrap();
    let loops = s.loops("file with spaces.ts").unwrap();
    assert_eq!(loops.len(), 1);
    assert_eq!(loops[0].message, "editor saved");
    assert_eq!(loops[0].kind, "save");
}
