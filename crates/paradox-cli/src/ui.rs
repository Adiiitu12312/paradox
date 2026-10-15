use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use clap::Parser;
use paradox_core::repo::Repo;

#[derive(Parser)]
#[command(name = "paradox ui", about = "Open a local, session-only Git workspace in your browser")]
struct Options {
    #[arg(long)]
    no_open: bool,
    #[arg(long, hide = true)]
    assets_dir: Option<PathBuf>,
}

pub fn run(args: &[OsString], index: usize) -> anyhow::Result<i32> {
    let options = match Options::try_parse_from(&args[index..]) {
        Ok(options) => options,
        Err(error) => {
            let code = error.exit_code();
            error.print()?;
            return Ok(code);
        }
    };
    let repo = Repo::discover(&args[..index])?;
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(paradox_ui::serve(
        repo,
        paradox_ui::UiOptions { assets_dir: options.assets_dir },
        move |ready| {
            println!("Paradox UI: {}", ready.url);
            println!("Close the browser tab or press Ctrl+C to stop.");
            if !options.no_open {
                if let Err(error) = open_browser(&ready.url) {
                    eprintln!("paradox: could not open browser: {error}; open the URL above");
                }
            }
        },
    ))?;
    println!("Paradox UI stopped.");
    Ok(0)
}

fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command.creation_flags(0x08000000);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = Command::new("xdg-open");
    command.arg(url).spawn()?;
    Ok(())
}
