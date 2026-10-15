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

#[cfg(windows)]
mod detached_handles {
    use std::ffi::c_void;
    use std::io;

    const HANDLE_FLAG_INHERIT: u32 = 1;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn GetHandleInformation(handle: *mut c_void, flags: *mut u32) -> i32;
        fn SetHandleInformation(handle: *mut c_void, mask: u32, flags: u32) -> i32;
    }

    /// The launcher can itself have inherited capture pipes. Redirecting the
    /// detached child's stdio does not stop CreateProcess from inheriting those
    /// original handles too, keeping its caller's output() blocked on EOF.
    /// Command duplicates the explicitly selected null/log handles separately.
    pub struct Guard(Vec<(*mut c_void, u32)>);

    impl Guard {
        pub fn new() -> io::Result<Self> {
            let mut guard = Self(Vec::new());
            for which in [-10i32, -11, -12] {
                // These borrowed standard handles remain owned by the process.
                let handle = unsafe { GetStdHandle(which as u32) };
                if handle.is_null() || handle as isize == -1 {
                    continue;
                }
                let mut flags = 0;
                if unsafe { GetHandleInformation(handle, &mut flags) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                if flags & HANDLE_FLAG_INHERIT != 0 {
                    guard.0.push((handle, flags));
                    if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
            }
            Ok(guard)
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            for &(handle, flags) in self.0.iter().rev() {
                // Restore on success, spawn error, or unwinding. Never close a
                // standard handle or change unrelated handle flag bits.
                unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, flags) };
            }
        }
    }
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
            #[cfg(windows)]
            let inherited_stdio = detached_handles::Guard::new()
                .context("could not isolate detached watch standard handles")?;
            let spawn = command.spawn();
            #[cfg(windows)]
            drop(inherited_stdio);
            let mut child = spawn.context("could not start watch session")?;
            #[cfg(unix)]
            ensure!(child.wait()?.success(), "watch detachment failed");
            #[cfg(windows)]
            {
                let _ = &mut child;
            }
            Ok(())
        })?;
        let state = paradox_core::watch::status(&repo)?.context("watch stopped during startup")?;
        let idle_notice = if state.idle_minutes == 0 {
            "idle exit disabled".to_string()
        } else {
            format!("stops after {} min idle", state.idle_minutes)
        };
        println!(
            "Paradox watching {} tracked files; {idle_notice}; background pid={}; use paradox watch --stop",
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
