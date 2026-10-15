use crate::blob::{atomic_write, directory, reject_link};
use crate::repo::Repo;
use crate::store::LoopStore;
use anyhow::{Context, Result, ensure};
use jsonc_parser::cst::{CstInputValue, CstRootNode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    let _ = path;
    Ok(())
}

/// Follow existing hook links, preserving native Git's executable eligibility.
pub fn runnable(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(windows)]
    {
        use std::io::Read;
        let mut prefix = [0; 2];
        fs::File::open(path)
            .and_then(|mut f| f.read_exact(&mut prefix))
            .is_ok()
            && matches!(&prefix, b"#!" | b"MZ")
    }
}

pub fn hooks_dir(repo: &Repo) -> Result<PathBuf> {
    let output = repo.git(&["rev-parse", "--path-format=absolute", "--git-path", "hooks"])?;
    Ok(PathBuf::from(
        std::str::from_utf8(&output.stdout)?.trim_end_matches(['\r', '\n']),
    ))
}

/// Git executes hooks through sh on all supported platforms, including Windows.
pub fn hook_script(exe: &Path, previous: Option<&Path>, event: Option<&str>) -> Result<String> {
    let mut script = String::from("#!/bin/sh\n# Paradox managed hook\n");
    if let Some(previous) = previous {
        script.push_str(&format!(
            "{} \"$@\" || exit $?\n",
            shell_quote(previous.to_str().context("hook path must be UTF-8")?)
        ));
    }
    if let Some(event) = event {
        script.push_str(&format!(
            "exec {} loop --hook {event}\n",
            shell_quote(exe.to_str().context("binary path must be UTF-8")?)
        ));
    }
    Ok(script)
}

#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    edits: Vec<Edit>,
    hooks: Vec<Hook>,
}
#[derive(Serialize, Deserialize)]
struct Edit {
    file: String,
    parents: Vec<String>,
    key: String,
    value: Value,
}
#[derive(Serialize, Deserialize)]
struct Hook {
    path: PathBuf,
    backup: Option<PathBuf>,
    script: String,
    #[serde(default)]
    owners: Vec<String>,
}

fn manifest_path(repo: &Repo) -> PathBuf {
    repo.store.join(format!("setup-{}.json", repo.worktree))
}
fn save_manifest(store: &LoopStore, manifest: &Manifest) -> Result<()> {
    write_manifest(store, &manifest_path(&store.repo), manifest)
}
fn write_manifest(store: &LoopStore, path: &Path, manifest: &Manifest) -> Result<()> {
    reject_link(path)?;
    if manifest.edits.is_empty() && manifest.hooks.is_empty() {
        if path.exists() {
            fs::remove_file(path)?;
            #[cfg(unix)]
            fs::File::open(&store.repo.store)?.sync_all()?;
        }
        return Ok(());
    }
    let bytes = serde_json::to_vec_pretty(manifest)?;
    let old = fs::metadata(path).map_or(0, |m| m.len().max(4096));
    let new = (bytes.len() as u64).max(4096);
    ensure!(
        new <= old
            || store.bytes()?.saturating_sub(old).saturating_add(new)
                <= store.config.store_cap_bytes,
        "store cap leaves no room for setup metadata; unpin and run loop --gc, or raise the cap"
    );
    atomic_write(path, &bytes)
}
fn load_manifest(repo: &Repo) -> Result<Manifest> {
    let path = manifest_path(repo);
    load_manifest_at(&path)
}
fn load_manifest_at(path: &Path) -> Result<Manifest> {
    reject_link(path)?;
    if path.exists() {
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    } else {
        Ok(Manifest::default())
    }
}
fn input(value: &Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(v) => CstInputValue::Bool(*v),
        Value::Number(v) => CstInputValue::Number(v.to_string()),
        Value::String(v) => CstInputValue::String(v.clone()),
        Value::Array(v) => CstInputValue::Array(v.iter().map(input).collect()),
        Value::Object(v) => {
            CstInputValue::Object(v.iter().map(|(k, v)| (k.clone(), input(v))).collect())
        }
    }
}

fn config_path(repo: &Repo, file: &str) -> Result<PathBuf> {
    ensure!(
        matches!(
            file,
            ".vscode/settings.json" | ".vscode/extensions.json" | ".claude/settings.json"
        ),
        "invalid managed config path"
    );
    let path = repo.root.join(file);
    reject_link(path.parent().context("missing settings directory")?)?;
    reject_link(&path)?;
    Ok(path)
}

fn edit_config(repo: &Repo, edit: &Edit, uninstall: bool) -> Result<bool> {
    let path = config_path(repo, &edit.file)?;
    if uninstall && !path.exists() {
        return Ok(false);
    }
    let before = if path.exists() {
        fs::read_to_string(&path)?
    } else {
        "{}\n".into()
    };
    let document = CstRootNode::parse(&before, &Default::default())?;
    let mut object = document
        .object_value_or_create()
        .context("settings root must be an object")?;
    for parent in &edit.parents {
        if uninstall {
            let Some(child) = object.object_value(parent) else {
                return Ok(false);
            };
            object = child;
        } else {
            object = object
                .object_value_or_create(parent)
                .context("existing settings property must be an object")?;
        }
    }
    let array = if uninstall {
        let Some(array) = object.array_value(&edit.key) else {
            return Ok(false);
        };
        array
    } else {
        object
            .array_value_or_create(&edit.key)
            .context("existing settings property must be an array")?
    };
    let existing = array
        .elements()
        .into_iter()
        .find(|node| node.to_serde_value().as_ref() == Some(&edit.value));
    if uninstall {
        let Some(existing) = existing else {
            return Ok(false);
        };
        existing.remove();
    } else {
        if existing.is_some() {
            return Ok(false);
        }
        array.append(input(&edit.value));
    }
    // Preserve all unrelated CST nodes, comments and trailing commas.
    let after = document.to_string();
    if path.exists() {
        ensure!(
            fs::read_to_string(&path)? == before,
            "settings changed concurrently; retry init"
        );
    }
    directory(path.parent().context("missing settings directory")?)?;
    atomic_write(&path, after.as_bytes())?;
    Ok(true)
}

pub fn install(repo: Repo, exe: &Path) -> Result<()> {
    let store = LoopStore::open(repo)?;
    let repo = &store.repo;
    let mut manifest = load_manifest(repo)?;
    let hook_manifest_path = repo.store.join("setup-hooks.json");
    let mut hook_manifest = load_manifest_at(&hook_manifest_path)?;
    let hooks = hooks_dir(repo)?;
    reject_link(&hooks)?;
    directory(&hooks)?;
    for event in ["pre-commit", "post-commit"] {
        let path = hooks.join(event);
        if let Some(record) = hook_manifest.hooks.iter_mut().find(|r| r.path == path) {
            ensure!(
                fs::read_to_string(&path).is_ok_and(|s| s == record.script),
                "managed hook was edited; preserve it and resolve before reinstalling"
            );
            let script = hook_script(
                exe,
                record.backup.as_deref().filter(|p| runnable(p)),
                Some(event),
            )?;
            if record.script != script {
                atomic_write(&path, script.as_bytes())?;
                executable(&path)?;
                record.script = script;
            }
            if !record.owners.contains(&repo.worktree) {
                record.owners.push(repo.worktree.clone());
            }
            write_manifest(&store, &hook_manifest_path, &hook_manifest)?;
            continue;
        }
        let existing = fs::symlink_metadata(&path).ok();
        ensure!(
            existing
                .as_ref()
                .is_none_or(|m| m.is_file() || m.file_type().is_symlink()),
            "hook must be a file"
        );
        let backup = existing
            .is_some()
            .then(|| hooks.join(format!("{event}.paradox-original")));
        if let Some(backup) = &backup {
            ensure!(
                !backup.exists(),
                "hook backup already exists; refusing to overwrite"
            );
        }
        let script = hook_script(
            exe,
            backup.as_deref().filter(|_| runnable(&path)),
            Some(event),
        )?;
        hook_manifest.hooks.push(Hook {
            path: path.clone(),
            backup: backup.clone(),
            script: script.clone(),
            owners: vec![repo.worktree.clone()],
        });
        write_manifest(&store, &hook_manifest_path, &hook_manifest)?;
        if let Some(backup) = backup {
            fs::rename(&path, backup)?;
        }
        atomic_write(&path, script.as_bytes())?;
        executable(&path)?;
    }
    let additions = [
        Edit {
            file: ".vscode/settings.json".into(),
            parents: vec!["emeraldwalk.runonsave".into()],
            key: "commands".into(),
            value: json!({"match":".*", "cmd":"paradox loop \"${file}\"", "isAsync":true}),
        },
        Edit {
            file: ".vscode/extensions.json".into(),
            parents: vec![],
            key: "recommendations".into(),
            value: json!("emeraldwalk.runonsave"),
        },
        Edit {
            file: ".claude/settings.json".into(),
            parents: vec!["hooks".into()],
            key: "PostToolUse".into(),
            value: json!({"matcher":"Edit|Write|MultiEdit", "hooks":[{"type":"command", "command":"paradox loop --hook claude"}]}),
        },
    ];
    for edit in additions {
        if manifest
            .edits
            .iter()
            .any(|e| e.file == edit.file && e.key == edit.key && e.value == edit.value)
        {
            edit_config(repo, &edit, false)?;
            continue;
        }
        // Determine whether the entry predates Paradox; never claim ownership of it.
        let path = config_path(repo, &edit.file)?;
        let before = if path.exists() {
            fs::read_to_string(&path)?
        } else {
            "{}".into()
        };
        let root = CstRootNode::parse(&before, &Default::default())?;
        let value = root.to_serde_value().context("invalid settings JSON")?;
        let mut parent = &value;
        for key in &edit.parents {
            parent = &parent[key];
        }
        if parent[&edit.key]
            .as_array()
            .is_some_and(|a| a.contains(&edit.value))
        {
            continue;
        }
        manifest.edits.push(edit);
        save_manifest(&store, &manifest)?;
        edit_config(repo, manifest.edits.last().context("missing edit")?, false)?;
    }
    Ok(())
}

pub fn uninstall(repo: Repo) -> Result<()> {
    let store = LoopStore::open(repo)?;
    let repo = &store.repo;
    let mut manifest = load_manifest(repo)?;
    for edit in &manifest.edits {
        edit_config(repo, edit, true)?;
    }
    manifest.edits.clear();
    let hook_manifest_path = repo.store.join("setup-hooks.json");
    let mut hook_manifest = load_manifest_at(&hook_manifest_path)?;
    let active = hooks_dir(repo)?;
    let mut retained = Vec::new();
    for mut hook in hook_manifest.hooks {
        if hook.path.parent() != Some(active.as_path()) {
            retained.push(hook);
            continue;
        }
        hook.owners.retain(|owner| owner != &repo.worktree);
        if !hook.owners.is_empty() {
            retained.push(hook);
            continue;
        }
        reject_link(&hook.path)?;
        if fs::read_to_string(&hook.path).is_ok_and(|s| s == hook.script) {
            fs::remove_file(&hook.path)?;
            if let Some(backup) = &hook.backup {
                fs::rename(backup, &hook.path)?;
            }
        } else {
            eprintln!(
                "paradox init: edited hook retained at {}",
                hook.path.display()
            );
            retained.push(hook);
        }
    }
    hook_manifest.hooks = retained;
    write_manifest(&store, &hook_manifest_path, &hook_manifest)?;
    save_manifest(&store, &manifest)?;
    Ok(())
}

pub fn agent_rule(guide: Option<&Path>) -> String {
    let mut rule = String::from(
        "Paradox agent rules:\nBefore each edit, write a fresh one-line message using the file's comment style followed by > on line 1 (line 2 after a shebang or <?php). Use /*> message */ for CSS and <!--> message --> for HTML/Markdown/templates; include the closing delimiter. JSON has no comment marker and is auto-labelled.\nUse paradox instead of git; commit with paradox commit. Never undo with raw git reset --hard, checkout -- or clean: inspect paradox loops, compare, then rewind.\nAfter creating a file, run paradox track <file>. If your tool has no post-edit hook, ensure paradox watch is running: explicitly start paradox watch --background, or tell the user if it cannot start. Nothing starts it automatically.\n",
    );
    if let Some(path) = guide {
        rule.push_str(&format!("Read the Paradox guide at {}\n", path.display()));
    }
    rule
}

pub fn editor_config(editor: &str) -> Result<&'static str> {
    match editor {
        "neovim" => Ok(
            "vim.api.nvim_create_autocmd('BufWritePost', { callback = function(ev) vim.fn.jobstart({'paradox', 'loop', vim.api.nvim_buf_get_name(ev.buf)}) end })",
        ),
        "jetbrains" => Ok(
            "File Watcher: Program=paradox; Arguments=loop \"$FilePath$\"; Working directory=$ProjectFileDir$; Trigger on external changes=true; Output paths to refresh=(empty).",
        ),
        "sublime" => Ok(
            r#"Install the existing Hooks package (https://github.com/twolfson/sublime-hooks).
Save this as Packages/User/Paradox.sublime-build, then select Tools > Build System > Paradox:
{"cmd":["paradox","loop","$file"],"working_dir":"$file_path"}
Merge this into Preferences.sublime-settings (or the corresponding project hook):
{"on_post_save_user":[{"command":"build","scope":"window"}]}
Sublime's build command launches the capture process asynchronously."#,
        ),
        _ => anyhow::bail!("editor must be neovim, jetbrains or sublime"),
    }
}
