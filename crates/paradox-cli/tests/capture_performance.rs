use paradox_core::repo::Repo;
use paradox_core::store::LoopStore;
use std::ffi::OsString;
use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
#[ignore = "release-only 50,000-file capture performance gate"]
fn fifty_thousand_files_single_save_and_warm_scan() {
    let root = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(root.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.path().join("no-global"))
            .env("GIT_AUTHOR_NAME", "Paradox Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Paradox Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "--initial-branch=main"]);
    git(&["config", "core.hooksPath", ".git/no-hooks"]);
    git(&["config", "core.autocrlf", "false"]);
    git(&["config", "commit.gpgsign", "false"]);
    for directory in 0..100 {
        let parent = root.path().join(format!("files-{directory:03}"));
        fs::create_dir(&parent).unwrap();
        for file in 0..500 {
            fs::write(parent.join(format!("file-{file:03}.ts")), b"base\n").unwrap();
        }
    }
    git(&["add", "-A"]);
    git(&["commit", "-qm", "performance fixture"]);
    let repo = Repo::discover(&[OsString::from("-C"), root.path().as_os_str().to_owned()]).unwrap();
    let mut store = LoopStore::open(repo).unwrap();
    assert_eq!(store.capture(&[]).unwrap().saved, 0);
    let mut scans = Vec::new();
    for _ in 0..5 {
        let started = Instant::now();
        let report = store.capture(&[]).unwrap();
        scans.push(started.elapsed());
        assert_eq!(report.hashed, 0);
    }
    scans.sort();
    println!(
        "50,000 tracked files: warm stat scan median={:?}, hashes=0",
        scans[2]
    );
    drop(store);
    let mut saves = Vec::new();
    for n in 0..29 {
        fs::write(
            root.path().join("files-000/file-000.ts"),
            format!("//> performance {n}\nvalue {n}\n"),
        )
        .unwrap();
        let started = Instant::now();
        assert!(
            Command::new(env!("CARGO_BIN_EXE_paradox"))
                .args(["loop", "files-000/file-000.ts"])
                .current_dir(root.path())
                .stdout(Stdio::null())
                .stderr(if n == 8 {
                    Stdio::inherit()
                } else {
                    Stdio::null()
                })
                .status()
                .unwrap()
                .success()
        );
        if n >= 8 {
            saves.push(started.elapsed());
        }
    }
    saves.sort();
    println!(
        "50,000 tracked files: single-file CLI save median={:?}, n=21",
        saves[10]
    );
    assert!(
        scans[2] < Duration::from_millis(500),
        "warm stat scan exceeds 500 ms"
    );
    assert!(
        saves[10] < Duration::from_millis(50),
        "single-file loop exceeds 50 ms"
    );
    let store = LoopStore::open(
        Repo::discover(&[OsString::from("-C"), root.path().as_os_str().to_owned()]).unwrap(),
    )
    .unwrap();
    assert_eq!(store.loops("files-000/file-000.ts").unwrap().len(), 29);
}
