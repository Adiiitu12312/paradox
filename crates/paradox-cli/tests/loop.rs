use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::ffi::OsString;
use paradox_core::repo::Repo;
use paradox_core::store::LoopStore;

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    for args in [&["init","-q","--initial-branch=main"][..], &["config","core.autocrlf","false"], &["config","core.hooksPath",".git/no-hooks"]] {
        assert!(Command::new("git").args(args).current_dir(temp.path()).status().unwrap().success());
    }
    fs::write(temp.path().join("file.ts"), "//> initial\nbody\n").unwrap();
    assert!(Command::new("git").args(["add","file.ts"]).current_dir(temp.path()).status().unwrap().success());
    temp
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_paradox"));
    command.current_dir(root).env("GIT_CONFIG_NOSYSTEM","1")
        .env("GIT_CONFIG_GLOBAL",root.join("no-global"));
    command
}

fn open(root: &Path) -> LoopStore {
    LoopStore::open(Repo::discover(&[OsString::from("-C"),root.as_os_str().to_owned()]).unwrap()).unwrap()
}

#[test]
fn twenty_concurrent_saves_deduplicate_and_keep_the_last_save() {
    let temp = fixture();
    let mut children = Vec::new();
    for _ in 0..20 {
        children.push(command(temp.path()).args(["loop","file.ts"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
    }
    for mut child in children { assert!(child.wait().unwrap().success()); }
    let store = open(temp.path());
    assert_eq!(store.loops("file.ts").unwrap().len(),1);
    drop(store);
    let final_bytes = b"//> final save\nlast complete bytes\n";
    fs::write(temp.path().join("file.ts"),final_bytes).unwrap();
    assert!(command(temp.path()).args(["loop","file.ts"]).status().unwrap().success());
    let store = open(temp.path());
    let latest = &store.loops("file.ts").unwrap()[0];
    use paradox_core::blob::BlobStore;
    assert_eq!(store.blobs.get(&latest.blob_hash).unwrap(),final_bytes);
    assert_eq!(latest.message,"final save");
    assert_eq!(fs::read(temp.path().join("file.ts")).unwrap(),final_bytes);
    assert_eq!(store.index.query_row("PRAGMA integrity_check",[],|r|r.get::<_,String>(0)).unwrap(),"ok");
}

#[test]
fn save_failures_are_nonfatal_and_metadata_is_excluded() {
    let temp = fixture();
    assert!(command(temp.path()).args(["loop","file.ts"]).status().unwrap().success());
    fs::write(temp.path().join(".paradox/config.toml"),"size_limit_bytes = 0\n").unwrap();
    assert!(command(temp.path()).args(["loop","file.ts"]).status().unwrap().success());
    assert!(fs::read_to_string(temp.path().join(".paradox/log")).unwrap().contains("positive"));
    let out = Command::new("git").args(["status","--porcelain","--untracked-files=all"]).current_dir(temp.path()).output().unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains(".paradox"));
    assert!(command(temp.path()).args(["loop","--help"]).status().unwrap().success());
}

#[cfg(feature = "fault-injection")]
#[test]
fn injected_process_death_recovers_atomic_blobs_and_wal_transactions() {
    use paradox_core::blob::BlobStore;
    for point in ["during_blob_write","after_blob","after_index_insert","after_gc_commit"] {
        let temp = fixture();
        assert!(command(temp.path()).args(["loop","file.ts"]).status().unwrap().success());
        fs::write(temp.path().join("file.ts"),format!("//> {point}\nnew bytes\n")).unwrap();
        let status = command(temp.path()).args(["loop","file.ts"]).env("PARADOX_TEST_CRASH_AT",point)
            .stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
        assert!(!status.success(),"{point} must terminate at the injected point");
        assert!(command(temp.path()).args(["loop","--gc"]).status().unwrap().success());
        assert!(command(temp.path()).args(["loop","file.ts"]).status().unwrap().success());
        let store = open(temp.path());
        assert_eq!(store.index.query_row("PRAGMA integrity_check",[],|r|r.get::<_,String>(0)).unwrap(),"ok");
        for item in store.loops("file.ts").unwrap() { assert!(store.blobs.get(&item.blob_hash).is_ok()); }
        let latest = store.loops("file.ts").unwrap().remove(0);
        assert_eq!(store.blobs.get(&latest.blob_hash).unwrap(),fs::read(temp.path().join("file.ts")).unwrap());
    }
}
