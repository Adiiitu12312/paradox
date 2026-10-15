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
            if let Some(file) = input["file_path"].as_str() { files.push(PathBuf::from(file)); }
            if let Some(edits) = input["edits"].as_array() {
                for edit in edits { if let Some(file) = edit["file_path"].as_str() { files.push(PathBuf::from(file)); } }
            }
            files.sort(); files.dedup();
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
                if flag == "--guide-path" { guide = Some(PathBuf::from(value)); }
                else { editor = Some(value.to_str().context("invalid editor name")?.to_string()); }
            }
            _ => rest.push(args[i].clone()),
        }
        i += 1;
    }
    if print_rule || editor.is_some() {
        ensure!(!uninstall && rest.is_empty(), "printing cannot be combined with setup options");
        if print_rule { print!("{}", paradox_core::setup::agent_rule(guide.as_deref())); }
        if let Some(editor) = editor { println!("{}", paradox_core::setup::editor_config(&editor)?); }
        return Ok(0);
    }
    ensure!(guide.is_none(), "--guide-path requires --print-agent-rule");
    let mut globals = args[..index].to_vec();
    if !rest.is_empty() || Repo::discover(&globals).is_err() {
        ensure!(!uninstall, "uninstall requires an existing repository");
        let mut command = globals.clone(); command.push("init".into()); command.extend(rest.iter().cloned());
        let status = paradox_core::git::command(&command).status()?;
        if !status.success() { return Ok(status.code().unwrap_or(1)); }
        // git init's optional directory is the last non-option after value flags.
        let mut target = None;
        let mut skip = false;
        for arg in &rest {
            if skip { skip = false; continue; }
            let text = arg.to_string_lossy();
            if matches!(text.as_ref(), "--template" | "--initial-branch" | "-b" | "--object-format" | "--ref-format" | "--separate-git-dir") { skip = true; }
            else if !text.starts_with('-') { target = Some(arg.clone()); }
        }
        if let Some(target) = target { globals.extend([OsString::from("-C"), target]); }
    }
    let repo = Repo::discover(&globals)?;
    if uninstall { paradox_core::setup::uninstall(repo)?; println!("Paradox hooks and owned editor/AI entries removed; loops retained."); }
    else { paradox_core::setup::install(repo, &std::env::current_exe()?)?; println!("Paradox initialized: editor/AI capture and Git hooks configured. Nothing is started automatically."); }
    Ok(0)
}

pub fn commit(args: &[OsString], index: usize, file: Option<usize>) -> Result<i32> {
    let globals = &args[..file.unwrap_or(index)];
    let repo = Repo::discover(globals)?;
    let mut commit_args = Vec::new();
    let mut paths = file.map(|f| vec![args[f].clone()]).unwrap_or_default();
    let mut stage_options = Vec::new();
    let mut no_verify = false;
    let mut after_dash = false;
    let mut i = index + 1;
    while i < args.len() {
        let arg = &args[i];
        let text = arg.to_string_lossy();
        if !after_dash && matches!(text.as_ref(), "-n" | "--no-verify") { no_verify = true; i += 1; continue; }
        commit_args.push(arg.clone());
        if after_dash { paths.push(arg.clone()); }
        else if text == "--" { after_dash = true; }
        else if text == "--pathspec-file-nul" || text.starts_with("--pathspec-from-file=") { stage_options.push(arg.clone()); }
        else if matches!(text.as_ref(), "-m" | "--message" | "-F" | "--file" | "-C" | "--reuse-message" | "-c" | "--reedit-message" | "--author" | "--date" | "--cleanup" | "--fixup" | "--squash" | "-t" | "--template" | "--trailer" | "--pathspec-from-file") {
            i += 1;
            let value = args.get(i).context("commit option requires a value")?;
            commit_args.push(value.clone());
            if text == "--pathspec-from-file" { stage_options.extend([arg.clone(), value.clone()]); }
        } else if !text.starts_with('-') { paths.push(arg.clone()); }
        i += 1;
    }
    if let Some(file) = file { commit_args.extend([OsString::from("--"), args[file].clone()]); }
    capture(repo.clone());
    let mut add = globals.to_vec(); add.extend([OsString::from("add"), OsString::from("-A")]);
    add.extend(stage_options);
    if !paths.is_empty() { add.push("--".into()); add.extend(paths); }
    let status = paradox_core::git::command(&add).status()?;
    if !status.success() { return Ok(status.code().unwrap_or(1)); }
    // Let Git prepare its own partial/-a index, then strip it in a mandatory
    // pre-commit step. --no-verify skips user hooks, but cannot leak markers.
    let temporary = tempfile::Builder::new().prefix("paradox-commit-hooks-").tempdir()?;
    let hooks = paradox_core::setup::hooks_dir(&repo)?;
    let exe = std::env::current_exe()?;
    if hooks.is_dir() {
        for entry in fs::read_dir(&hooks)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() { continue; }
            let name = entry.file_name();
            if name == "pre-commit" || name == "post-commit" || (no_verify && name == "commit-msg") { continue; }
            let script = paradox_core::setup::hook_script(&exe, Some(&entry.path()), None)?;
            let path = temporary.path().join(name);
            fs::write(&path, script)?; paradox_core::setup::executable(&path)?;
        }
    }
    for event in ["pre-commit", "post-commit"] {
        let previous = hooks.join(event);
        let previous = previous.is_file().then_some(previous);
        let previous = if no_verify && event == "pre-commit" { None } else { previous.as_deref() };
        let script = paradox_core::setup::hook_script(&exe, previous, Some(event))?;
        let path = temporary.path().join(event);
        fs::write(&path, script)?; paradox_core::setup::executable(&path)?;
    }
    let mut command = globals.to_vec();
    command.extend([OsString::from("-c"), format!("core.hooksPath={}", temporary.path().display()).into(), OsString::from("commit")]);
    command.extend(commit_args);
    let status = paradox_core::git::command(&command).status()?;
    if status.success() { purge(globals); }
    Ok(status.code().unwrap_or(1))
}
