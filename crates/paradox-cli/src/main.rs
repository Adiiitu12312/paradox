use std::ffi::OsString;
use std::io;

use clap::Parser;
use paradox_cli::dispatch::{Route, force_git_args, route};

#[derive(Parser)]
#[command(name = "paradox", disable_version_flag = true, about = "Git with local per-file loops (M1 development build)", after_help = "Everything else is Git: original arguments, terminal I/O and exit status pass through.\n\nPlanned additions (not available in M1):\n  loop, loops, compare, rewind, timeline, pin, unpin, track, untrack, ui\n  commit   Auto-stage then commit (M4)\n  init     Set up Git and automatic loop capture (M4)\n\nUse `paradox git <args>` to force pure Git, including commit and init.\nUse `paradox git --help` for Git help. Do not alias git yet.")]
struct Help {}

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let result = match route(&args) {
        Route::Help => {
            Help::parse_from([OsString::from("paradox"), OsString::from("--help")]);
            return;
        }
        Route::Git => paradox_core::git::passthrough(&args),
        Route::ForceGit { index } => paradox_core::git::passthrough(&force_git_args(&args, index)),
        Route::Owned { index } | Route::FileCommit { index, .. } => {
            eprintln!("paradox: '{}' is not available in M1; see paradox --help", args[index].to_string_lossy());
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
