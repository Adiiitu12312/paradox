use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use paradox_core::history::{absolute_time, clock_time, relative_time};
use paradox_core::repo::Repo;
use paradox_core::store::LoopStore;
use std::ffi::{OsStr, OsString};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

#[derive(Parser)]
struct FileArgs { file: PathBuf }

#[derive(Parser)]
#[command(about = "Compare working, HEAD or per-file loop versions")]
struct CompareArgs {
    file: PathBuf,
    a: Option<String>,
    b: Option<String>,
}

#[derive(Parser)]
#[command(about = "Restore a file with an undoable safety loop, or preview a batch")]
struct RewindArgs {
    file: Option<PathBuf>,
    version: Option<String>,
    #[arg(long, value_name = "HH:MM", conflicts_with_all = ["file", "version"])]
    at: Option<String>,
    #[arg(long, requires = "at")]
    yes: bool,
}

#[derive(Parser)]
struct PinArgs { id: i64 }

#[derive(Parser)]
struct TrackArgs {
    #[arg(required = true)]
    paths: Vec<PathBuf>,
}

fn open(args: &[OsString], index: usize) -> Result<LoopStore> {
    LoopStore::open(Repo::discover(&args[..index])?)
}

fn printable(text: &str) -> String {
    text.chars().flat_map(|c| if c.is_control() { c.escape_default().collect::<Vec<_>>() } else { vec![c] }).collect()
}

fn fallback(store: &mut LoopStore) {
    if let Err(error) = store.capture(&[]) { store.log(&format!("fallback scan: {error:#}")); }
}

fn print_diff(diff: &str) {
    let color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() && std::env::var("TERM").as_deref() != Ok("dumb");
    for line in diff.split_inclusive('\n') {
        let code = if line.starts_with('+') && !line.starts_with("+++") { "32" }
            else if line.starts_with('-') && !line.starts_with("---") { "31" }
            else if line.starts_with("@@") { "36" } else { "" };
        if color && !code.is_empty() { print!("\x1b[{code}m{line}\x1b[0m"); }
        else { print!("{line}"); }
    }
}

pub fn available(command: &OsStr) -> bool {
    ["loops", "compare", "rewind", "pin", "unpin", "track", "untrack"].iter().any(|name| command == *name)
}

pub fn run(args: &[OsString], index: usize) -> i32 {
    match execute(args, index) {
        Ok(()) => 0,
        Err(error) => {
            if let Some(error) = error.downcast_ref::<clap::Error>() {
                let _ = error.print();
                error.exit_code()
            } else {
                eprintln!("paradox {}: {error:#}", args[index].to_string_lossy());
                1
            }
        }
    }
}

fn execute(args: &[OsString], index: usize) -> Result<()> {
    match args[index].to_str().context("invalid command")? {
        "loops" => {
            let options = FileArgs::try_parse_from(&args[index..])?;
            let mut store = open(args, index)?;
            let path = store.repo.relative(&options.file)?;
            fallback(&mut store);
            println!("Loops for {path:?} (newest first)");
            for record in store.loops(&path)? {
                println!("{}  {}  {}  {}{}{}  {}  head={}", record.id, absolute_time(record.ts), relative_time(record.ts), record.kind,
                    if record.pinned { " pinned" } else { "" }, if record.deleted { " absent" } else { "" }, printable(&record.message),
                    if record.head_sha.is_empty() { "unborn" } else { &record.head_sha[..record.head_sha.len().min(12)] });
            }
            let head = store.head_version(&path)?;
            if store.repo.head.is_empty() { println!("HEAD  no commit (file absent)"); }
            else { println!("HEAD  {}  committed baseline{}", absolute_time(head.ts), if head.deleted { " (file absent)" } else { "" }); }
        }
        "compare" => {
            let options = CompareArgs::try_parse_from(&args[index..])?;
            let mut store = open(args, index)?;
            let path = store.repo.relative(&options.file)?;
            store.refresh_git_changes()?;
            // Explicit versions are read before the fallback scan can evict
            // older unpinned content. No working-file write is involved.
            let diff = match (&options.a, &options.b) {
                (None, None) => {
                    fallback(&mut store);
                    store.compare(&path, &store.latest_version(&path)?, &store.resolve(&path, "working")?)?
                }
                (Some(a), b) => {
                    let diff = store.compare(&path, &store.resolve(&path, a)?, &store.resolve(&path, b.as_deref().unwrap_or("working"))?)?;
                    fallback(&mut store);
                    diff
                }
                _ => bail!("provide the first version before the second"),
            };
            print_diff(&diff);
        }
        "pin" | "unpin" => {
            let options = PinArgs::try_parse_from(&args[index..])?;
            let mut store = open(args, index)?;
            let pinned = args[index] == "pin";
            // Protect the requested loop before a fallback scan may evict it.
            store.pin_loop(options.id, pinned)?;
            fallback(&mut store);
            println!("Loop {} {}", options.id, if pinned { "pinned" } else { "unpinned" });
        }
        "track" | "untrack" => {
            let options = TrackArgs::try_parse_from(&args[index..])?;
            let mut store = open(args, index)?;
            let remove = args[index] == "untrack";
            let changed = store.track_paths(&options.paths, remove)?;
            fallback(&mut store);
            println!("{} {changed} explicit path(s){}", if remove { "Untracked" } else { "Tracked" }, if remove { "; Git-tracked files remain tracked" } else { "" });
        }
        "rewind" => {
            let options = RewindArgs::try_parse_from(&args[index..])?;
            if let Some(time) = options.at {
                let at = clock_time(&time)?;
                let mut store = open(args, index)?;
                let preview = store.preview_at(at)?;
                println!("Dry run: {} file(s) to {}", preview.plans.len(), absolute_time(at));
                for plan in &preview.plans { println!("  {} {:?} -> {}", if plan.version.deleted { "delete" } else { "restore" }, plan.path, plan.version.label); }
                for (path, reason) in &preview.skipped { eprintln!("Skipped {path:?}: {}", printable(reason)); }
                // Do not hold the repository lock while waiting for the user.
                // Targets are validated again after reopening on confirmation.
                drop(store);
                if preview.plans.is_empty() { return Ok(()); }
                if !options.yes {
                    print!("Apply these file changes? Type yes: ");
                    io::stdout().flush()?;
                    let mut answer = String::new();
                    io::stdin().read_line(&mut answer)?;
                    ensure!(answer.trim() == "yes", "rewind cancelled; no working files changed");
                }
                let mut store = open(args, index)?;
                for (path, id) in store.apply_rewinds(&preview.plans)? { println!("Rewound {path:?}; undo uses safety loop {id}"); }
            } else {
                let file = options.file.context("provide a file and version, or --at HH:MM")?;
                let version = options.version.context("provide a loop ID, relative time, HEAD or undo")?;
                let mut store = open(args, index)?;
                let id = store.rewind_file(&file, &version)?;
                println!("Rewound {file:?}; undo uses safety loop {id}");
            }
        }
        _ => bail!("command is not available"),
    }
    Ok(())
}
