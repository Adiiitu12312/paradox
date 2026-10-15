use std::ffi::OsString;
use std::io;

use clap::Parser;
use paradox_cli::dispatch::{Route, force_git_args, route};

mod commands;
mod workflow;

#[derive(Parser)]
#[command(name = "paradox loop", about = "Save loops of tracked files and exit")]
struct LoopArgs {
    files: Vec<std::path::PathBuf>,
    #[arg(long)]
    gc: bool,
    #[arg(long)]
    stats: bool,
    #[arg(long, hide = true, conflicts_with_all = ["files", "gc", "stats"])]
    hook: Option<String>,
}

fn capture(args: &[OsString], index: usize) {
    let started = std::time::Instant::now();
    let profile = std::env::var_os("PARADOX_PROFILE_CAPTURE").is_some();
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
    if let Some(event) = &options.hook {
        if let Err(error) = workflow::hook(event, &args[..index]) {
            paradox_core::store::log_at(&repo.store, &format!("{event}: {error:#}"));
            if event == "pre-commit" {
                eprintln!("paradox: staged marker stripping failed: {error:#}");
                std::process::exit(1);
            }
        }
        return;
    }
    let log_root = repo.store.clone();
    if profile {
        eprintln!("capture profile: discovery {:?}", started.elapsed());
    }
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut store = paradox_core::store::LoopStore::open(repo)?;
        if profile {
            eprintln!("capture profile: opened {:?}", started.elapsed());
        }
        if !options.files.is_empty() || (!options.gc && !options.stats) {
            let _ = store.capture(&options.files)?;
        }
        if options.gc {
            store.purge()?;
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
    if profile {
        eprintln!("capture profile: finished {:?}", started.elapsed());
    }
    if let Err(error) = result {
        paradox_core::store::log_at(&log_root, &format!("{error:#}"));
    }
}

#[derive(Parser)]
#[command(
    name = "paradox",
    disable_version_flag = true,
    about = "Git with local per-file loops (M4 development build)",
    after_help = "Everything else is Git: original arguments, terminal I/O and exit status pass through.\n\nAvailable additions:\n  loop [file...]     Save tracked files; --gc cleans storage; --stats reports usage\n  loops <file>       List per-file loops and the HEAD baseline\n  compare <file> [a] [b]   Compare IDs, relative times, messages, HEAD or working\n  rewind <file> <version>  Restore one file; undo restores its safety loop\n  rewind --at HH:MM [--yes]  Preview and confirm a batch\n  pin <id>, unpin <id>     Protect a loop from eviction\n  track <path...>, untrack <path...>  Explicit paths beyond Git's tracked set\n\nPlanned: commit/init (M4), manual watch (M4.5), timeline (M5), ui (M6).\nUse `paradox git <args>` to force pure Git, including commit and init.\nUse `paradox git --help` for Git help. Do not alias git yet."
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
        Route::Owned { index } if commands::available(&args[index]) => {
            Ok(commands::run(&args, index))
        }
        Route::Owned { index } if args[index] == "init" || args[index] == "commit" => {
            let result = if args[index] == "init" {
                workflow::init(&args, index)
            } else {
                workflow::commit(&args, index, None)
            };
            Ok(workflow_result(result))
        }
        Route::FileCommit { index, file } => {
            Ok(workflow_result(workflow::commit(&args, index, Some(file))))
        }
        Route::Owned { index } => {
            eprintln!(
                "paradox: '{}' is not available in M4; see paradox --help",
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

fn workflow_result(result: anyhow::Result<i32>) -> i32 {
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("paradox: {error:#}");
            1
        }
    }
}
