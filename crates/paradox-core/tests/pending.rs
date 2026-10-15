use paradox_core::{repo::Repo, store::LoopStore};
use std::{
    fs,
    path::Path,
    process::Command,
    sync::{Arc, Barrier},
    time::{Duration, Instant},
};
fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("no-global"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn repo(root: &Path) -> Repo {
    Repo::discover(&["-C".into(), root.as_os_str().to_owned()]).unwrap()
}
#[test]
fn twenty_contended_savers_queue_paths_and_holder_drains_exact_final_content() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-q"]);
    fs::write(root.join("a.ts"), "base\n").unwrap();
    git(root, &["add", "a.ts"]);
    let mut holder = LoopStore::open(repo(root)).unwrap();
    holder.capture(&[]).unwrap();
    let barrier = Arc::new(Barrier::new(21));
    let mut threads = Vec::new();
    for n in 0..20 {
        let repo = repo(root);
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            let started = Instant::now();
            assert!(
                LoopStore::try_open(repo.clone(), Duration::from_millis(15))
                    .unwrap()
                    .is_none()
            );
            let waiting = started.elapsed();
            paradox_core::pending::enqueue(&repo, &["a.ts".into()]).unwrap();
            (n, waiting, started.elapsed())
        }));
    }
    barrier.wait();
    let mut waits = Vec::new();
    let mut totals = Vec::new();
    for thread in threads {
        let (_, wait, total) = thread.join().unwrap();
        waits.push(wait);
        totals.push(total);
    }
    waits.sort();
    totals.sort();
    println!(
        "pending handoff: median lock wait {:?}, marker exit {:?}, max {:?}",
        waits[10], totals[10], totals[19]
    );
    assert!(
        waits[10] < Duration::from_millis(50),
        "lock wait exceeds save budget"
    );
    assert!(
        {
            let row = include_str!("../../../benchmarks/pending.csv")
                .lines()
                .find(|line| line.starts_with(&format!("{},", std::env::consts::OS)));
            let ceiling = row.map_or(50.0, |row| {
                let fields: Vec<_> = row.split(',').collect();
                let baseline: f64 = fields[1].parse().unwrap();
                let limit: f64 = fields[2].parse().unwrap();
                limit.min(baseline * 1.2 + 1.0)
            });
            println!("pending median regression ceiling: {ceiling} ms");
            totals[10] < Duration::from_secs_f64(ceiling / 1000.0)
        },
        "contended saves exceed their stored OS median exit budget"
    );
    fs::write(
        root.join("a.ts"),
        "//> final concurrent edit\nfinal state\n",
    )
    .unwrap();
    paradox_core::pending::enqueue(&holder.repo, &["a.ts".into()]).unwrap();
    drop(holder);
    let store = LoopStore::open(repo(root)).unwrap();
    let loops = store.loops("a.ts").unwrap();
    assert_eq!(
        loops
            .iter()
            .filter(|item| item.message == "final concurrent edit")
            .count(),
        1
    );
    assert_eq!(
        paradox_core::blob::BlobStore::get(&store.blobs, &loops[0].blob_hash).unwrap(),
        b"//> final concurrent edit\nfinal state\n"
    );
    assert_eq!(
        fs::read_dir(store.repo.store.join("pending"))
            .unwrap()
            .count(),
        0
    );
}
#[test]
fn pending_handoff_from_another_worktree_uses_that_worktrees_current_file() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Pending Test"]);
    git(root, &["config", "user.email", "test@example.invalid"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["config", "core.hooksPath", ".git/no-hooks"]);
    fs::write(root.join("a.ts"), "base\n").unwrap();
    git(root, &["add", "a.ts"]);
    git(root, &["commit", "-qm", "base"]);
    let secondary = tempfile::tempdir().unwrap();
    let worktree = secondary.path().join("worktree");
    git(
        root,
        &[
            "worktree",
            "add",
            "-qb",
            "topic",
            worktree.to_str().unwrap(),
        ],
    );
    let holder = LoopStore::open(repo(root)).unwrap();
    fs::write(worktree.join("a.ts"), "//> other worktree\nother content\n").unwrap();
    paradox_core::pending::enqueue(&repo(&worktree), &["a.ts".into()]).unwrap();
    drop(holder);
    let store = LoopStore::open(repo(&worktree)).unwrap();
    assert_eq!(store.loops("a.ts").unwrap()[0].message, "other worktree");
    drop(store);
    let primary = LoopStore::open(repo(root)).unwrap();
    assert!(primary.loops("a.ts").unwrap().is_empty());
}
