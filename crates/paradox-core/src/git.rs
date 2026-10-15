use std::ffi::OsString;
use std::io;
use std::process::Command;

/// Inherit the caller's environment and terminal handles without any repo work.
pub fn command(args: &[OsString]) -> Command {
    let mut command = Command::new("git");
    command.args(args);
    command
}

/// Unix replaces Paradox entirely, retaining Git's pager, editor and signals.
#[cfg(unix)]
pub fn passthrough(args: &[OsString]) -> io::Result<i32> {
    use std::os::unix::process::CommandExt;
    Err(command(args).exec())
}

#[cfg(windows)]
pub fn passthrough(args: &[OsString]) -> io::Result<i32> {
    // Both processes remain in the same console group. Let Git receive Ctrl+C,
    // while the wrapper waits and returns Git's exact 32-bit process exit code.
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }
    unsafe extern "system" fn console_handler(event: u32) -> i32 {
        i32::from(event == 0 || event == 1)
    }
    // A custom handler (unlike the NULL/ignore mode) is not inherited by Git.
    unsafe { SetConsoleCtrlHandler(Some(console_handler), 1) };
    let result = command(args).status().map(|s| s.code().unwrap_or(1));
    unsafe { SetConsoleCtrlHandler(Some(console_handler), 0) };
    result
}
