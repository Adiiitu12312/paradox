use std::ffi::OsString;
use std::io;
use std::process::Command;

/// Capture bounded stdout/stderr concurrently, terminating on output overflow.
pub fn output_bounded(mut command: Command, input: Option<&[u8]>, limit: usize) -> io::Result<std::process::Output> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    command.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdin = child.stdin.take();
    std::thread::scope(|scope| {
        let (sender, receiver) = std::sync::mpsc::channel();
        for (kind, mut reader) in [(0, Box::new(stdout) as Box<dyn Read + Send>), (1, Box::new(stderr) as Box<dyn Read + Send>)] {
            let sender = sender.clone();
            scope.spawn(move || {
                let mut bytes = Vec::new();
                let result = reader.by_ref().take(limit.saturating_add(1) as u64).read_to_end(&mut bytes);
                let _ = sender.send((kind, bytes, result));
            });
        }
        drop(sender);
        let writer = scope.spawn(move || -> io::Result<()> {
            if let (Some(mut stdin), Some(bytes)) = (stdin, input) { stdin.write_all(bytes)?; }
            Ok(())
        });
        let mut outputs = [Vec::new(), Vec::new()];
        let mut failure = None;
        for _ in 0..2 {
            match receiver.recv() {
                Ok((kind, bytes, result)) => {
                    if bytes.len() > limit { failure = Some(io::Error::other("Git output exceeds limit")); }
                    if let Err(error) = result { failure = Some(error); }
                    outputs[kind] = bytes;
                    if failure.is_some() { let _ = child.kill(); }
                }
                Err(_) => { let _ = child.kill(); failure = Some(io::Error::other("Git output reader failed")); break; }
            }
        }
        let status = child.wait()?;
        let written = writer.join().map_err(|_| io::Error::other("Git input writer failed"))?;
        if let Some(error) = failure { return Err(error); }
        if status.success() { written?; }
        let [stdout, stderr] = outputs;
        Ok(std::process::Output { status, stdout, stderr })
    })
}

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
