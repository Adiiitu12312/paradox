use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn median<T: Ord + Copy>(values: &mut [T]) -> T {
    values.sort_unstable();
    values[values.len() / 2]
}

fn persistent_regression(rounds: [bool; 3]) -> bool {
    rounds.into_iter().filter(|exceeded| *exceeded).count() >= 2
}

#[test]
fn isolated_noisy_round_does_not_fail_but_two_exceedances_do() {
    for rounds in [[true, false, false], [false, true, false], [false, false, true], [false; 3]] {
        assert!(!persistent_regression(rounds));
    }
    for rounds in [[true, true, false], [true, false, true], [false, true, true], [true; 3]] {
        assert!(persistent_regression(rounds));
    }
    assert_eq!(median(&mut [-5_i128, 1, -3]), -3);
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
fn passthrough_overhead_within_stored_baseline() {
    // Windows has no exec: inherited-console spawn/wait is required for parity.
    // G3 allows <20 ms there; Unix retains <5 ms. Also catch regressions against
    // the stored observation, with 50% + 1 ms scheduling allowance, capped by G3.
    let row = include_str!("../../../benchmarks/passthrough.csv")
        .lines()
        .find(|line| line.starts_with(&format!("{},", std::env::consts::OS)))
        .expect("missing OS performance baseline");
    let fields: Vec<_> = row.split(',').collect();
    let baseline_ms: f64 = fields[1].parse().unwrap();
    let limit_ms: f64 = fields[2].parse().unwrap();
    let ceiling_ms = limit_ms.min(baseline_ms * 1.5 + 1.0);
    let ceiling = Duration::from_secs_f64(ceiling_ms / 1000.0);
    println!(
        "OS limit={limit_ms} ms; stored baseline={baseline_ms} ms; regression ceiling={ceiling_ms} ms"
    );
    let root = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(root.path())
            .status()
            .unwrap()
            .success()
    );
    let mut below_limit = true;
    for args in [&["--version"][..], &["status", "--porcelain=v2", "-z"]] {
        let mut exceeded = [false; 3];
        for (round, exceeded) in exceeded.iter_mut().enumerate() {
        let mut direct = Vec::new();
        let mut wrapped = Vec::new();
        let mut differences = Vec::new();
        for _ in 0..16 {
            elapsed("git", root.path(), args);
            elapsed(env!("CARGO_BIN_EXE_paradox"), root.path(), args);
        }
        // Alternate ordering to reduce bias from scheduling and filesystem caches.
        for n in 0..201 {
            let (a, b) = if (n + round) % 2 == 0 {
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
            "{args:?}: round={}/3, git median={git:?}, paradox median={paradox:?}, paired overhead={overhead:?}, samples_each=201, ceiling_ms={ceiling_ms}", round + 1
        );
        *exceeded = overhead > ceiling;
        }
        let failed = persistent_regression(exceeded);
        println!("{args:?}: exceeded={exceeded:?}, persistent_regression={failed}");
        below_limit &= !failed;
    }
    assert!(
        below_limit,
        "pass-through median paired overhead exceeded its stored OS regression ceiling in at least two of three consecutive rounds"
    );
}
