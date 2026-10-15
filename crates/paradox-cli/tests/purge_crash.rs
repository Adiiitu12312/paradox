#![cfg(feature = "fault-injection")]
use paradox_core::{blob::BlobStore, repo::Repo, store::LoopStore};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
};

fn command(root: &Path, exe: &str) -> Command {
    let mut c = Command::new(exe);
    c.current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("no-global"))
        .env("GIT_AUTHOR_NAME", "Paradox Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Paradox Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z");
    c
}
fn git(root: &Path, args: &[&str]) {
    let o = command(root, "git").args(args).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
fn open(root: &Path) -> LoopStore {
    LoopStore::open(Repo::discover(&["-C".into(), root.as_os_str().to_owned()]).unwrap()).unwrap()
}

#[test]
fn death_during_or_after_purge_is_repairable_without_losing_pins() {
    for point in ["during_purge", "after_purge_commit"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        git(root, &["init", "-q"]);
        git(root, &["config", "commit.gpgsign", "false"]);
        git(root, &["config", "core.autocrlf", "false"]);
        fs::write(root.join("a.ts"), "base\n").unwrap();
        git(root, &["add", "a.ts"]);
        git(root, &["commit", "-qm", "base"]);
        let mut pin = 0;
        for n in 0..6 {
            fs::write(root.join("a.ts"), format!("version {n}\n")).unwrap();
            {
                let mut s = open(root);
                s.capture(&[]).unwrap();
                if n == 0 {
                    pin = s.loops("a.ts").unwrap()[0].id;
                    s.pin_loop(pin, true).unwrap();
                }
            }
            git(root, &["add", "a.ts"]);
            git(root, &["commit", "-qm", &format!("commit {n}")]);
        }
        let status = command(root, env!("CARGO_BIN_EXE_paradox"))
            .args(["loop", "--gc"])
            .env("PARADOX_TEST_CRASH_AT", point)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success(), "fault was not reached: {point}");
        let status = command(root, env!("CARGO_BIN_EXE_paradox"))
            .args(["loop", "--gc"])
            .status()
            .unwrap();
        assert!(status.success());
        let s = open(root);
        assert_eq!(
            s.index
                .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        let loops = s.loops("a.ts").unwrap();
        assert!(loops.iter().any(|l| l.id == pin && l.pinned));
        for l in loops {
            assert!(s.blobs.exists(&l.blob_hash).unwrap());
        }
    }
}
