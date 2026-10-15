use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn median<T: Ord + Copy>(values: &mut [T]) -> T {
    values.sort_unstable();
    values[values.len() / 2]
}

fn elapsed(exe: &str, root: &std::path::Path, args: &[&str]) -> Duration {
    let started = Instant::now();
    let status = Command::new(exe)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    started.elapsed()
}

/// Run explicitly against release builds; ordinary unit tests do not time debug code.
#[test]
#[ignore = "release-only performance gate: cargo test --release --test performance -- --ignored --nocapture"]
fn passthrough_overhead_below_five_ms() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(root.path())
            .status()
            .unwrap()
            .success()
    );
    for args in [&["--version"][..], &["status", "--porcelain=v2", "-z"]] {
        let mut direct = Vec::new();
        let mut wrapped = Vec::new();
        let mut differences = Vec::new();
        for _ in 0..8 {
            elapsed("git", root.path(), args);
            elapsed(env!("CARGO_BIN_EXE_paradox"), root.path(), args);
        }
        // Alternate ordering to reduce bias from scheduling and filesystem caches.
        for n in 0..101 {
            let (a, b) = if n % 2 == 0 {
                (
                    elapsed("git", root.path(), args),
                    elapsed(env!("CARGO_BIN_EXE_paradox"), root.path(), args),
                )
            } else {
                let b = elapsed(env!("CARGO_BIN_EXE_paradox"), root.path(), args);
                (elapsed("git", root.path(), args), b)
            };
            direct.push(a);
            wrapped.push(b);
            differences.push(b.as_nanos() as i128 - a.as_nanos() as i128);
        }
        let git = median(&mut direct);
        let paradox = median(&mut wrapped);
        // Keep signed paired differences until after the median, so scheduling
        // noise cannot bias the result by clamping individual negative samples.
        let overhead = Duration::from_nanos(median(&mut differences).max(0) as u64);
        println!(
            "{args:?}: git median={git:?}, paradox median={paradox:?}, paired overhead={overhead:?}, n=101"
        );
        assert!(
            overhead < Duration::from_millis(5),
            "pass-through overhead {overhead:?} exceeds 5 ms"
        );
    }
}
