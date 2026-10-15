#![cfg(feature = "fault-injection")]

use paradox_core::blob::BlobStore;
use paradox_core::repo::Repo;
use paradox_core::store::LoopStore;
use std::ffi::OsString;
use std::fs;
use std::process::{Command, Stdio};

#[test]
fn gc_death_preserves_pins_and_reclaims_orphans_on_retry() {
    for point in ["during_gc", "after_gc_commit"] {
        let temp = tempfile::tempdir().unwrap();
        for args in [&["init", "-q"][..], &["config", "core.autocrlf", "false"]] {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(temp.path())
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let call = |fault: Option<&str>| {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_paradox"));
            cmd.current_dir(temp.path())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Some(fault) = fault {
                cmd.env("PARADOX_TEST_CRASH_AT", fault);
            }
            cmd
        };
        for seed in 1..=6u32 {
            let mut state = seed;
            let bytes: Vec<_> = (0..100_000)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    33 + (state % 94) as u8
                })
                .collect();
            fs::write(temp.path().join("file.ts"), bytes).unwrap();
            if seed == 1 {
                assert!(
                    Command::new("git")
                        .args(["add", "file.ts"])
                        .current_dir(temp.path())
                        .status()
                        .unwrap()
                        .success()
                );
            }
            assert!(
                call(None)
                    .args(["loop", "file.ts"])
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let repo =
            Repo::discover(&[OsString::from("-C"), temp.path().as_os_str().to_owned()]).unwrap();
        let store = LoopStore::open(repo.clone()).unwrap();
        let loops = store.loops("file.ts").unwrap();
        assert_eq!(loops.len(), 6);
        let pin = loops.last().unwrap().id;
        store
            .index
            .execute("UPDATE loops SET pinned=1 WHERE id=?1", [pin])
            .unwrap();
        drop(store);
        fs::write(repo.store.join("config.toml"), "store_cap_bytes=320000\n").unwrap();
        assert!(
            !call(Some(point))
                .args(["loop", "--gc"])
                .status()
                .unwrap()
                .success()
        );
        let store = LoopStore::open(repo.clone()).unwrap();
        if point == "during_gc" {
            assert_eq!(store.loops("file.ts").unwrap().len(), 6);
        }
        for item in store.loops("file.ts").unwrap() {
            store.blobs.get(&item.blob_hash).unwrap();
        }
        drop(store);
        assert!(
            call(None)
                .args(["loop", "--gc"])
                .status()
                .unwrap()
                .success()
        );
        let store = LoopStore::open(repo).unwrap();
        let loops = store.loops("file.ts").unwrap();
        assert!(loops.len() < 6);
        assert!(loops.iter().any(|l| l.id == pin && l.pinned));
        assert!(store.bytes().unwrap() <= 320000);
        for item in loops {
            store.blobs.get(&item.blob_hash).unwrap();
        }
    }
}
