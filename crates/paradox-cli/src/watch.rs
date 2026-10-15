use anyhow::{Context, Result, ensure};
use clap::Parser;
use paradox_core::{repo::Repo, store::LoopStore};
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::process::{Command, Stdio};

#[derive(Parser)]
#[command(
    name = "paradox watch",
    about = "Manually watch tracked files for this session; never starts automatically"
)]
struct Options {
    #[arg(long, conflicts_with_all=["stop", "status", "session_child"])]
    background: bool,
    #[arg(long, conflicts_with_all=["background", "status", "session_child"])]
    stop: bool,
    #[arg(long, conflicts_with_all=["background", "stop", "session_child"])]
    status: bool,
    #[arg(long, hide = true)]
    session_child: Option<String>,
}

#[cfg(unix)]
fn detach() -> Result<()> {
    // This runs in a fresh single-threaded child, before notify or Ctrl+C
    // installs worker threads. Both intermediate processes exit immediately.
    unsafe {
        let first = libc::fork();
        ensure!(first >= 0, "watch fork failed");
        if first > 0 {
            libc::_exit(0);
        }
        ensure!(libc::setsid() >= 0, "watch setsid failed");
        let second = libc::fork();
        ensure!(second >= 0, "watch second fork failed");
        if second > 0 {
            libc::_exit(0);
        }
    }
    Ok(())
}

pub fn run(args: &[OsString], index: usize) -> Result<i32> {
    let options = match Options::try_parse_from(&args[index..]) {
        Ok(options) => options,
        Err(error) => {
            let code = error.exit_code();
            error.print()?;
            return Ok(code);
        }
    };
    let repo = Repo::discover(&args[..index])?;
    if options.status {
        match paradox_core::watch::status(&repo)? {
            Some(state) => println!(
                "running pid={} uptime_seconds={} loops_created={}",
                state.pid,
                paradox_core::store::now()?
                    .saturating_sub(state.started_ms)
                    .max(0)
                    / 1000,
                state.loops_created
            ),
            None => println!("not running"),
        }
        return Ok(0);
    }
    if options.stop {
        println!(
            "{}",
            if paradox_core::watch::stop(&repo)? {
                "Paradox watch stop requested"
            } else {
                "Paradox watch is not running"
            }
        );
        return Ok(0);
    }
    if options.background {
        // Initialization is explicit here; unrelated commands never call this.
        drop(LoopStore::open(repo.clone())?);
        ensure!(
            paradox_core::watch::status(&repo)?.is_none(),
            "Paradox watch is already running in this repository"
        );
        let token = paradox_core::watch::session_token()?;
        let output_path = repo.store.join("watch-output.log");
        ensure!(
            !std::fs::symlink_metadata(&output_path).is_ok_and(|m| m.file_type().is_symlink()),
            "watch output must not be a symlink"
        );
        let output = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(output_path)?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(&args[..index])
            .args([
                OsString::from("watch"),
                OsString::from("--session-child"),
                OsString::from(&token),
            ])
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x00000008 | 0x00000200);
        }
        paradox_core::watch::wait_started(&repo, &token, || {
            let mut child = command.spawn().context("could not start watch session")?;
            #[cfg(unix)]
            ensure!(child.wait()?.success(), "watch detachment failed");
            #[cfg(windows)]
            {
                let _ = &mut child;
            }
            Ok(())
        })?;
        let state = paradox_core::watch::status(&repo)?.context("watch stopped during startup")?;
        println!(
            "Paradox watching {} tracked files in background; pid={}; use paradox watch --stop",
            state.tracked_files, state.pid
        );
        return Ok(0);
    }
    let token = if let Some(token) = options.session_child {
        #[cfg(unix)]
        detach()?;
        token
    } else {
        paradox_core::watch::session_token()?
    };
    paradox_core::watch::run(repo, token)?;
    Ok(0)
}
