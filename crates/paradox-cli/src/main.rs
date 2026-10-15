use std::ffi::OsString;
use std::io;

use clap::Parser;
use paradox_cli::dispatch::{Route, force_git_args, route};

#[derive(Parser)]
#[command(name = "paradox loop", about = "Save loops of tracked files and exit")]
struct LoopArgs {
    files: Vec<std::path::PathBuf>,
    #[arg(long)]
    gc: bool,
    #[arg(long)]
    stats: bool,
}

fn capture(args: &[OsString], index: usize) {
    // Editor and AI save commands must never fail their caller.
    let options = match LoopArgs::try_parse_from(&args[index..]) {
        Ok(options) => options,
        Err(error) => {
            let _ = error.print();
            return;
        }
    };
    let repo = match paradox_core::repo::Repo::discover(&args[..index]) {
        Ok(repo) => repo,
        Err(error) => {
            eprintln!("paradox loop: {error:#}");
            return;
        }
    };
    let log_root = repo.store.clone();
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut store = paradox_core::store::LoopStore::open(repo)?;
        if !options.files.is_empty() || (!options.gc && !options.stats) {
            let _ = store.capture(&options.files)?;
        }
        if options.gc {
            store.gc()?;
        }
        if options.stats {
            let count: i64 = store
                .index
                .query_row("SELECT COUNT(*) FROM loops", [], |r| r.get(0))?;
            let oldest: Option<i64> =
                store
                    .index
                    .query_row("SELECT MIN(ts) FROM loops", [], |r| r.get(0))?;
            println!(
                "loops={count} bytes={} oldest_ms={}",
                store.bytes()?,
                oldest
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "none".into())
            );
        }
        Ok(())
    })();
    if let Err(error) = result {
        paradox_core::store::log_at(&log_root, &format!("{error:#}"));
    }
}

#[derive(Parser)]
#[command(
    name = "paradox",
    disable_version_flag = true,
    about = "Git with local per-file loops (M1 development build)",
    after_help = "Everything else is Git: original arguments, terminal I/O and exit status pass through.\n\nPlanned additions (not available in M1):\n  loop, loops, compare, rewind, timeline, pin, unpin, track, untrack, ui\n  commit   Auto-stage then commit (M4)\n  init     Set up Git and automatic loop capture (M4)\n\nUse `paradox git <args>` to force pure Git, including commit and init.\nUse `paradox git --help` for Git help. Do not alias git yet."
)]
struct Help {}

fn main() {
    // Raw OS argv routing comes first. Clap is constructed only for owned help;
    // pass-through never initializes a store, configuration, or logging.
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let result = match route(&args) {
        Route::Help => {
            Help::parse_from([OsString::from("paradox"), OsString::from("--help")]);
            return;
        }
        Route::Git => paradox_core::git::passthrough(&args),
        Route::ForceGit { index } => paradox_core::git::passthrough(&force_git_args(&args, index)),
        Route::Owned { index } if args[index] == "loop" => {
            capture(&args, index);
            return;
        }
        Route::Owned { index } | Route::FileCommit { index, .. } => {
            eprintln!(
                "paradox: '{}' is not available in M1; see paradox --help",
                args[index].to_string_lossy()
            );
            std::process::exit(2);
        }
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            if error.kind() == io::ErrorKind::NotFound {
                eprintln!("paradox: Git executable not found; install Git and put it on PATH");
            } else {
                eprintln!("paradox: could not run Git: {error}");
            }
            std::process::exit(127);
        }
    }
}
