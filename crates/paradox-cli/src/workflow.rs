use anyhow::{Context, Result, ensure};
use paradox_core::{repo::Repo, store::LoopStore};
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::PathBuf;

fn capture(repo: Repo) {
    let log = repo.store.clone();
    if let Err(error) = LoopStore::open(repo).and_then(|mut s| s.capture(&[]).map(|_| ())) {
        paradox_core::store::log_at(&log, &format!("fallback scan: {error:#}"));
    }
}
fn purge(globals: &[OsString]) {
    if let Ok(repo) = Repo::discover(globals) {
        let log = repo.store.clone();
        if let Err(error) = LoopStore::open(repo).and_then(|mut s| s.purge().map(|_| ())) {
            paradox_core::store::log_at(&log, &format!("post-commit purge: {error:#}"));
        }
    }
}

pub fn hook(event: &str, globals: &[OsString]) -> Result<()> {
    match event {
        "pre-commit" => {
            let repo = Repo::discover(globals)?;
            capture(repo.clone());
            // Failure must stop this commit rather than leak a message marker.
            paradox_core::commit::strip_staged(&repo)?;
        }
        "post-commit" => purge(globals),
        "claude" => {
            let mut bytes = Vec::new();
            std::io::stdin().take(8_388_609).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 8_388_608, "AI hook input exceeds 8 MiB");
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            let input = &value["tool_input"];
            let mut files = Vec::new();
            if let Some(file) = input["file_path"].as_str() {
                files.push(PathBuf::from(file));
            }
            if let Some(edits) = input["edits"].as_array() {
                for edit in edits {
                    if let Some(file) = edit["file_path"].as_str() {
                        files.push(PathBuf::from(file));
                    }
                }
            }
            files.sort();
            files.dedup();
            if !files.is_empty() {
                let mut store = LoopStore::open(Repo::discover(globals)?)?;
                store.capture(&files)?;
            }
        }
        _ => anyhow::bail!("unknown internal hook event"),
    }
    Ok(())
}

pub fn init(args: &[OsString], index: usize) -> Result<i32> {
    let mut rest = Vec::new();
    let mut uninstall = false;
    let mut print_rule = false;
    let mut editor = None;
    let mut guide = None;
    let mut i = index + 1;
    while i < args.len() {
        match args[i].to_str() {
            Some("--uninstall") => uninstall = true,
            Some("--print-agent-rule") => print_rule = true,
            Some("--print-editor-config" | "--guide-path") => {
                let flag = args[i].to_str().context("invalid init flag")?;
                i += 1;
                let value = args.get(i).context("init option requires a value")?;
                if flag == "--guide-path" {
                    guide = Some(PathBuf::from(value));
                } else {
                    editor = Some(value.to_str().context("invalid editor name")?.to_string());
                }
            }
            _ => rest.push(args[i].clone()),
        }
        i += 1;
    }
    if print_rule || editor.is_some() {
        ensure!(
            !uninstall && rest.is_empty(),
            "printing cannot be combined with setup options"
        );
        if print_rule {
            print!("{}", paradox_core::setup::agent_rule(guide.as_deref()));
        }
        if let Some(editor) = editor {
            println!("{}", paradox_core::setup::editor_config(&editor)?);
        }
        return Ok(0);
    }
    ensure!(guide.is_none(), "--guide-path requires --print-agent-rule");
    let mut globals = args[..index].to_vec();
    if !rest.is_empty() || Repo::discover(&globals).is_err() {
        ensure!(!uninstall, "uninstall requires an existing repository");
        let mut command = globals.clone();
        command.push("init".into());
        command.extend(rest.iter().cloned());
        let status = paradox_core::git::command(&command).status()?;
        if !status.success() {
            return Ok(status.code().unwrap_or(1));
        }
        // git init's optional directory is the last non-option after value flags.
        let mut target = None;
        let mut skip = false;
        for arg in &rest {
            if skip {
                skip = false;
                continue;
            }
            let text = arg.to_string_lossy();
            if matches!(
                text.as_ref(),
                "--template"
                    | "--initial-branch"
                    | "-b"
                    | "--object-format"
                    | "--ref-format"
                    | "--separate-git-dir"
            ) {
                skip = true;
            } else if !text.starts_with('-') {
                target = Some(arg.clone());
            }
        }
        if let Some(target) = target {
            globals.extend([OsString::from("-C"), target]);
        }
    }
    let repo = Repo::discover(&globals)?;
    if uninstall {
        paradox_core::setup::uninstall(repo)?;
        println!("Paradox hooks and owned editor/AI entries removed; loops retained.");
    } else {
        paradox_core::setup::install(repo, &std::env::current_exe()?)?;
        println!(
            "Paradox initialized: editor/AI capture and Git hooks configured. Nothing is started automatically."
        );
    }
    Ok(0)
}

pub fn commit(args: &[OsString], index: usize, file: Option<usize>) -> Result<i32> {
    let globals = &args[..file.unwrap_or(index)];
    let repo = Repo::discover(globals)?;
    let mut commit_args = file.map(|f| vec![args[f].clone()]).unwrap_or_default();
    let mut paths = file.map(|f| vec![args[f].clone()]).unwrap_or_default();
    let mut stage_options = Vec::new();
    let mut pathspec_input = None;
    let mut no_verify = false;
    let mut after_dash = false;
    let mut separator = None;
    let mut i = index + 1;
    while i < args.len() {
        let original = &args[i];
        let normalized;
        let arg = if !after_dash {
            if let Some(text) = original.to_str() {
                if text.starts_with("--no-veri") && "--no-verify".starts_with(text) {
                    no_verify = true;
                    i += 1;
                    continue;
                }
                if (text.starts_with("--veri") && "--verify".starts_with(text))
                    || text == "--no-no-verify"
                {
                    no_verify = false;
                    i += 1;
                    continue;
                }
                if text.starts_with('-') && !text.starts_with("--") {
                    let mut flags = String::from("-");
                    let mut chars = text[1..].chars();
                    while let Some(flag) = chars.next() {
                        if flag == 'n' {
                            no_verify = true;
                            continue;
                        }
                        flags.push(flag);
                        if matches!(flag, 'm' | 'F' | 'C' | 'c' | 't' | 'S' | 'u') {
                            flags.extend(chars);
                            break;
                        }
                    }
                    if flags == "-" && text != "-" {
                        i += 1;
                        continue;
                    }
                    normalized = OsString::from(flags);
                    &normalized
                } else {
                    original
                }
            } else {
                original
            }
        } else {
            original
        };
        let text = arg.to_string_lossy();
        if !after_dash && matches!(text.as_ref(), "-n" | "--no-verify") {
            no_verify = true;
            i += 1;
            continue;
        }
        if !after_dash && text == "--" {
            separator = Some(commit_args.len());
        }
        commit_args.push(arg.clone());
        if after_dash {
            paths.push(arg.clone());
        } else if text == "--" {
            after_dash = true;
        } else if text == "--pathspec-file-nul" || text.starts_with("--pathspec-from-file=") {
            if text == "--pathspec-from-file=-" {
                let temporary = pathspec_stdin()?;
                let replacement = OsString::from(format!(
                    "--pathspec-from-file={}",
                    temporary.path().display()
                ));
                *commit_args.last_mut().context("missing pathspec option")? = replacement.clone();
                stage_options.push(replacement);
                pathspec_input = Some(temporary);
            } else {
                stage_options.push(arg.clone());
            }
        } else if matches!(
            text.as_ref(),
            "-m" | "--message"
                | "-F"
                | "--file"
                | "-C"
                | "--reuse-message"
                | "-c"
                | "--reedit-message"
                | "--author"
                | "--date"
                | "--cleanup"
                | "--fixup"
                | "--squash"
                | "-t"
                | "--template"
                | "--trailer"
                | "--pathspec-from-file"
        ) || (text.starts_with('-') && !text.starts_with("--") && {
            let mut chars = text[1..].chars();
            let mut needs_value = false;
            while let Some(flag) = chars.next() {
                if matches!(flag, 'm' | 'F' | 'C' | 'c' | 't') {
                    needs_value = chars.next().is_none();
                    break;
                }
                if matches!(flag, 'S' | 'u') {
                    break;
                }
            }
            needs_value
        }) {
            i += 1;
            let value = args.get(i).context("commit option requires a value")?;
            commit_args.push(value.clone());
            if text == "--pathspec-from-file" {
                if value == "-" {
                    let temporary = pathspec_stdin()?;
                    let replacement = temporary.path().as_os_str().to_owned();
                    *commit_args.last_mut().context("missing pathspec value")? =
                        replacement.clone();
                    stage_options.extend([arg.clone(), replacement]);
                    pathspec_input = Some(temporary);
                } else {
                    stage_options.extend([arg.clone(), value.clone()]);
                }
            }
        } else if !text.starts_with('-') {
            paths.push(arg.clone());
        }
        i += 1;
    }
    // An unrecognized Git spelling must not disable our own stripper.
    commit_args.insert(separator.unwrap_or(commit_args.len()), "--verify".into());
    let outcome = paradox_core::commit::execute_commit(
        repo,
        &std::env::current_exe()?,
        commit_args,
        paradox_core::commit::StagePolicy::Auto { paths, options: stage_options },
        no_verify,
        false,
    )?;
    drop(pathspec_input);
    Ok(outcome.code)
}

fn pathspec_stdin() -> Result<tempfile::NamedTempFile> {
    let mut file = tempfile::Builder::new()
        .prefix("paradox-pathspec-")
        .tempfile()?;
    std::io::copy(&mut std::io::stdin().lock(), &mut file)?;
    file.as_file().sync_all()?;
    Ok(file)
}
