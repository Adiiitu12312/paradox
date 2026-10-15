use super::{fields, text};
use crate::{
    repo::Repo,
    store::{LoopStore, NewLoop},
    ui_git::{ActionError, Git},
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

struct Conflict {
    value: Value,
    stages: [Option<(String, String)>; 3],
    working: Vec<u8>,
}
fn inspect(git: &mut Git, path: &str) -> Result<Conflict> {
    let path = git.path(path)?;
    let entries = git.checked(
        &[
            "--literal-pathspecs",
            "ls-files",
            "--unmerged",
            "-z",
            "--",
            &path,
        ],
        None,
    )?;
    let mut stages: [Option<(String, String)>; 3] = [None, None, None];
    let mut hash = blake3::Hasher::new();
    hash.update(&entries);
    for row in fields(&entries) {
        let metadata = row
            .split(|b| *b == b'\t')
            .next()
            .context("invalid conflict record")?;
        let parts: Vec<_> = metadata.split(|b| *b == b' ').collect();
        ensure!(parts.len() == 3, "invalid stage record");
        let stage = std::str::from_utf8(parts[2])?.parse::<usize>()?;
        ensure!((1..=3).contains(&stage), "invalid conflict stage");
        stages[stage - 1] = Some((text(parts[0]), text(parts[1])));
    }
    ensure!(
        stages.iter().any(Option::is_some),
        "file has no unresolved index stages"
    );
    let mut rendered = Vec::new();
    let mut binary = false;
    for stage in &stages {
        if let Some((mode, sha)) = stage {
            let size = git.checked(&["cat-file", "-s", sha], None)?;
            ensure!(
                text(&size).trim().parse::<u64>()? <= 8_388_608,
                "conflict blob exceeds UI size limit"
            );
            let bytes = git.checked(&["cat-file", "blob", sha], None)?;
            let is_binary =
                bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() || mode == "160000";
            binary |= is_binary;
            rendered.push(json!({"text":if is_binary{String::new()}else{text(&bytes)},"binary":is_binary,"mode":mode}));
        } else {
            rendered.push(Value::Null);
        }
    }
    let (working, present, truncated) = crate::ui_git::worktree(&git.repo, &path)?;
    ensure!(!truncated, "conflict working file exceeds UI size limit");
    let work_binary = working.contains(&0) || std::str::from_utf8(&working).is_err();
    binary |= work_binary;
    hash.update(&working);
    hash.update(&[u8::from(present)]);
    let token = hash.finalize().to_hex().to_string();
    Ok(Conflict {
        value: json!({"path":path,"token":token,"base":rendered[0],"ours":rendered[1],"theirs":rendered[2],"result":if work_binary{String::new()}else{text(&working)},"deleted":!present,"binary":binary}),
        stages,
        working,
    })
}
pub fn conflict(repo: Repo, path: &str) -> Result<Value> {
    let mut git = Git::new(repo);
    let data = inspect(&mut git, path)?;
    Ok(git.finish(data.value))
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    Ours,
    Theirs,
    Manual,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveRequest {
    pub path: String,
    pub token: String,
    pub choice: Choice,
    pub result: Option<String>,
    #[serde(default)]
    pub mark_resolved: bool,
}
pub fn resolve_conflict(repo: Repo, request: ResolveRequest) -> Result<Value> {
    let repo=repo.refreshed_head()?;
    let mut git = Git::new(repo.clone());
    let current = inspect(&mut git, &request.path)?;
    if current.value["token"].as_str() != Some(request.token.as_str()) {
        return Err(ActionError {
            code: "stale_diff",
            message: "Conflict changed since it was opened; refresh before resolving".into(),
            commands: git.commands,
        }
        .into());
    }
    let stage = match request.choice {
        Choice::Ours => Some(1),
        Choice::Theirs => Some(2),
        Choice::Manual => None,
    };
    let bytes = if let Some(stage) = stage {
        if let Some((mode, sha)) = &current.stages[stage] {
            ensure!(
                mode == "100644" || mode == "100755",
                "Resolve linked/submodule conflicts with native Git in the terminal"
            );
            Some(git.checked(&["cat-file", "blob", sha], None)?)
        } else {
            None
        }
    } else {
        Some(
            request
                .result
                .context("manual resolution requires result text")?
                .into_bytes(),
        )
    };
    ensure!(
        bytes.as_ref().is_none_or(|bytes| bytes.len() <= 8_388_608),
        "resolution exceeds UI size limit"
    );
    let mut store = LoopStore::open(repo.clone())?;
    ensure!(
        !store.denied(&request.path),
        "conflict path cannot be protected by safety loops; resolve it with native Git in the terminal"
    );
    let protectable = current.working.len() as u64 <= store.config.size_limit_bytes
        && !current.working.iter().take(8192).any(|b| *b == 0);
    let mut recoverable_blob = None;
    if !protectable && stage.is_some() {
        for (_, sha) in current.stages.iter().flatten() {
            if git.checked(&["cat-file", "blob", sha], None)? == current.working {
                recoverable_blob = Some(sha.clone());
                break;
            }
        }
    }
    ensure!(
        protectable || recoverable_blob.is_some(),
        "conflict working bytes cannot be safely protected and differ from every recoverable Git stage; resolve it with native Git in the terminal"
    );
    let refreshed = inspect(&mut git, &request.path)?;
    ensure!(
        refreshed.value["token"] == current.value["token"],
        "conflict changed before safety capture"
    );
    let hash = blake3::hash(&current.working).to_hex().to_string();
    let mode = repo
        .regular_file(&request.path)?
        .as_ref()
        .and_then(crate::store::file_mode);
    let id = if protectable {
        Some(store.save_loop(NewLoop {
            path: &request.path,
            bytes: &current.working,
            hash: &hash,
            state: None,
            kind: "safety",
            message: "before conflict resolution",
            auto_label: false,
            deleted: current.value["deleted"] == true,
            pinned: true,
            mode,
        })?)
    } else {
        None
    };
    super::safety::write_result(&mut store, &request.path, bytes.as_deref())?;
    #[cfg(unix)]
    if bytes.is_some()
        && let Some(stage) = stage
        && let Some((mode, _)) = &current.stages[stage]
    {
        use std::os::unix::fs::PermissionsExt;
        let target = repo.root.join(&request.path);
        std::fs::set_permissions(
            &target,
            std::fs::Permissions::from_mode(if mode == "100755" { 0o755 } else { 0o644 }),
        )?;
        std::fs::File::open(target)?.sync_all()?;
    }
    // Only index conflict stages are marked resolved after the replacement is
    // durably present. No client-controlled patch or pathspec is ever evaluated.
    if request.mark_resolved {
        git.checked(
            &["--literal-pathspecs", "add", "-A", "--", &request.path],
            None,
        )?;
    }
    if let Some(id) = id {
        store.pin_loop(id, false)?;
        store
            .index
            .execute("DELETE FROM operation_pins WHERE id=?1", [id])?;
    }
    let safety_loops: Vec<_> = id
        .into_iter()
        .map(|id| json!({"path":request.path,"source":"working","id":id}))
        .collect();
    Ok(git.finish(json!({"changed":true,"safety_loops":safety_loops,"recoverable_git_blob":recoverable_blob,"resolved":request.mark_resolved})))
}
