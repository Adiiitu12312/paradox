use super::{Action, ResetMode, fields, text};
use crate::{
    config::Config,
    repo::Repo,
    store::{LoopStore, NewLoop},
    ui_git::{ActionError, Git},
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, io::Read};

#[derive(Clone, Debug, Serialize)]
pub struct Protection {
    pub path: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Loss {
    pub path: Option<String>,
    pub reference: Option<String>,
    pub description: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub action: Action,
    pub fingerprint: String,
    pub destructive: bool,
    pub losses: Vec<Loss>,
    pub safety: Vec<Protection>,
    pub skipped: Vec<Protection>,
    pub reflog_hints: Vec<String>,
    pub commands: Vec<crate::commit::CommandRecord>,
    #[serde(skip)]
    pub(super) observed_lease: Option<String>,
    #[serde(skip)]
    paths: Vec<String>,
}
pub(super) fn destructive(action: &Action) -> bool {
    matches!(
        action,
        Action::BranchDelete { .. }
            | Action::TagDelete { .. }
            | Action::BranchSwitch { .. }
            | Action::StashDrop { .. }
            | Action::StashApply { pop: true, .. }
            | Action::StashCreate { .. }
            | Action::Reset { .. }
            | Action::Discard { .. }
            | Action::Push {
                force_with_lease: true,
                ..
            }
            | Action::Abort { .. }
            | Action::Merge { .. }
            | Action::Rebase { .. }
            | Action::Pull { .. }
            | Action::CherryPick { .. }
            | Action::Revert { .. }
    )
}

fn selected_paths(git: &mut Git, action: &Action) -> Result<Vec<String>> {
    let mut paths = BTreeSet::new();
    if let Action::Discard { paths: selected } = action {
        ensure!(!selected.is_empty(), "select at least one path");
        for path in selected {
            let path = git.path(path)?;
            let data = git.checked(
                &[
                    "--literal-pathspecs",
                    "ls-files",
                    "--cached",
                    "--others",
                    "-z",
                    "--",
                    &path,
                ],
                None,
            )?;
            for name in fields(&data) {
                paths.insert(std::str::from_utf8(name)?.to_owned());
            }
            if paths.is_empty() {
                paths.insert(path);
            }
        }
    } else {
        let data = crate::ui_git::status(git.repo.clone())?;
        for file in data["files"].as_array().context("invalid status")? {
            if file["untracked"] != true {
                paths.insert(file["path"].as_str().context("invalid path")?.to_owned());
            }
        }
        if matches!(
            action,
            Action::StashCreate {
                include_untracked: true,
                ..
            }
        ) {
            let data = git.checked(&["ls-files", "--others", "--exclude-standard", "-z"], None)?;
            for name in fields(&data) {
                paths.insert(std::str::from_utf8(name)?.into());
            }
        }
        if let Action::Reset {
            target,
            mode: ResetMode::Hard,
        } = action
        {
            let target = super::revision(git, target)?;
            let changed =
                git.checked(&["diff", "--name-only", "-z", "HEAD", &target, "--"], None)?;
            for path in fields(&changed) {
                let path = std::str::from_utf8(path)?;
                let data = git.checked(
                    &[
                        "--literal-pathspecs",
                        "ls-files",
                        "--others",
                        "-z",
                        "--",
                        path,
                    ],
                    None,
                )?;
                for name in fields(&data) {
                    paths.insert(std::str::from_utf8(name)?.into());
                }
            }
        }
    }
    Ok(paths.into_iter().collect())
}
fn digest_file(path: &std::path::Path, hasher: &mut blake3::Hasher) -> Result<()> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            hasher.update(b"absent");
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let mut bytes = [0; 65_536];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        hasher.update(&bytes[..n]);
    }
    Ok(())
}
fn fingerprint(git: &mut Git, paths: &[String]) -> Result<String> {
    let mut hash = blake3::Hasher::new();
    for args in [
        &["status", "--porcelain=v2", "-z", "--untracked-files=normal"][..],
        &["for-each-ref", "--format=%(refname)%00%(objectname)"][..],
        &["rev-parse", "--git-path", "index"][..],
    ] {
        let data = git.checked(args, None)?;
        hash.update(&(data.len() as u64).to_le_bytes());
        hash.update(&data);
    }
    let index = git.checked(
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        None,
    )?;
    digest_file(std::path::Path::new(text(&index).trim_end()), &mut hash)?;
    hash.update(git.head()?.unwrap_or_default().as_bytes());
    for path in paths {
        hash.update(path.as_bytes());
        match crate::ui_git::worktree(&git.repo, path) {
            Ok((bytes, present, truncated)) => {
                hash.update(&[u8::from(present), u8::from(truncated)]);
                hash.update(&bytes);
                if let Ok(meta) = fs::symlink_metadata(git.repo.root.join(path)) {
                    hash.update(&meta.len().to_le_bytes());
                    if let Ok(stamp) = crate::store::stamp(&meta) {
                        hash.update(&stamp.0.to_le_bytes());
                    }
                }
            }
            Err(error) => {
                hash.update(error.to_string().as_bytes());
            }
        }
    }
    Ok(hash.finalize().to_hex().to_string())
}
fn snapshot(git: &mut Git, path: &str, source: &str) -> Result<(Vec<u8>, bool, Option<u32>)> {
    if source == "working" {
        let metadata = git.repo.regular_file(path)?;
        let (bytes, present, truncated) = crate::ui_git::worktree(&git.repo, path)?;
        ensure!(!truncated, "file exceeds UI text limit");
        if present {
            ensure!(
                metadata.is_some(),
                "linked files cannot be protected by a regular-file safety loop"
            );
        }
        return Ok((
            bytes,
            !present,
            metadata.as_ref().and_then(crate::store::file_mode),
        ));
    }
    let spec = format!(":0:{path}");
    let exists = git.run(&["cat-file", "-e", &spec], None)?;
    if !exists.status.success() {
        return Ok((Vec::new(), true, None));
    }
    let size = git.checked(&["cat-file", "-s", &spec], None)?;
    ensure!(
        text(&size).trim().parse::<u64>()? <= 8_388_608,
        "staged file exceeds UI text limit"
    );
    let entry = git.checked(
        &[
            "--literal-pathspecs",
            "ls-files",
            "--stage",
            "-z",
            "--",
            path,
        ],
        None,
    )?;
    let mode = fields(&entry)
        .first()
        .and_then(|entry| entry.split(|b| *b == b' ').next())
        .map(text)
        .unwrap_or_default();
    ensure!(
        mode != "120000" && mode != "160000",
        "linked/submodule index entries cannot be protected by regular-file loops"
    );
    let mode = if mode == "100755" {
        Some(0o755)
    } else {
        Some(0o644)
    };
    Ok((
        git.checked(&["cat-file", "blob", &spec], None)?,
        false,
        mode,
    ))
}

fn reference_object(git:&mut Git,reference:&str)->Result<String>{
    let bytes=git.checked(&["rev-parse","--verify","--end-of-options",reference],None)?;
    Ok(text(&bytes).trim().to_owned())
}

fn commit_losses(git:&mut Git,old:&str,new:Option<&str>,exclude_ref:Option<&str>,losses:&mut Vec<Loss>)->Result<()> {
    let mut revisions=vec![old.to_owned(),"--not".into()];
    if let Some(new)=new{revisions.push(new.into());}
    if let Some(reference)=exclude_ref{revisions.extend([format!("--exclude={reference}"),"--all".into()]);}
    let mut count_args=vec!["rev-list".to_owned(),"--count".into()];count_args.extend(revisions.clone());count_args.push("--".into());
    let count=git.checked(&count_args.iter().map(String::as_str).collect::<Vec<_>>(),None)?;
    let count=text(&count).trim().parse::<u64>()?;
    losses.push(Loss{path:None,reference:Some(old.into()),description:format!("{count} commits leave this reference's reachable history; up to 50 are listed below. Other references may still retain them.")});
    if count==0{return Ok(());}
    let mut args=vec!["log".to_owned(),"--no-show-signature".into(),"--no-notes".into(),"-z".into(),"--format=%H%x00%s".into(),"--max-count=50".into()];args.extend(revisions);args.push("--".into());
    let bytes=git.checked(&args.iter().map(String::as_str).collect::<Vec<_>>(),None)?;
    let mut records:Vec<_>=bytes.split(|byte|*byte==0).collect();if records.last()==Some(&b"".as_slice()){records.pop();}
    ensure!(records.len().is_multiple_of(2),"invalid loss-preview commit records");
    for row in records.as_chunks::<2>().0.iter(){losses.push(Loss{path:None,reference:Some(text(row[0])),description:format!("Commit: {}",text(row[1]))});}
    if count>50{losses.push(Loss{path:None,reference:None,description:format!("{} additional commits omitted from this bounded preview (total {count}); inspect the old object ID before confirming.",count-50)});}
    Ok(())
}

fn exact_losses(git:&mut Git,action:&Action,lease:Option<&str>,losses:&mut Vec<Loss>,hints:&mut Vec<String>)->Result<()> {
    match action{
        Action::BranchDelete{name,..}=>{
            let reference=format!("refs/heads/{name}");let old=reference_object(git,&reference)?;
            losses.push(Loss{path:None,reference:Some(reference.clone()),description:format!("Delete branch {name} at exact tip {old}")});
            if let Some(head)=git.head()?{let count=git.checked(&["rev-list","--count",&old,"--not",&head,"--"],None)?;losses.push(Loss{path:None,reference:Some(reference.clone()),description:format!("{} commits are not merged into the current HEAD",text(&count).trim())});}
            commit_losses(git,&old,None,Some(&reference),losses)?;
            hints.push(format!("Restore exact branch with argv {}",json!(["git","update-ref",reference,old])));
        },
        Action::TagDelete{name}=>{
            let reference=format!("refs/tags/{name}");let old=reference_object(git,&reference)?;
            losses.push(Loss{path:None,reference:Some(reference.clone()),description:format!("Delete tag object/reference {old}; tags normally have no reflog")});
            let commit=git.run(&["rev-parse","--verify","--quiet","--end-of-options",&format!("{reference}^{{commit}}")],None)?;
            if commit.status.success(){commit_losses(git,text(&commit.stdout).trim(),None,Some(&reference),losses)?;}
            hints.push(format!("Restore the original annotated/lightweight tag with argv {}",json!(["git","update-ref",reference,old])));
        },
        Action::Reset{target,mode}=>{
            let old=git.head()?.context("reset requires an existing HEAD")?;let new=super::revision(git,target)?;
            losses.push(Loss{path:None,reference:Some("HEAD".into()),description:format!("Move HEAD from {old} to {new}; reset mode {mode:?}")});
            commit_losses(git,&old,Some(&new),None,losses)?;
            hints.push(format!("Old HEAD remains inspectable with git show {old}; recover its history with git branch recovery-{short} {old}",short=&old[..12]));
        },
        Action::Push{remote,branch,force_with_lease:true}=>{
            let old=lease.context("missing observed remote tip")?;let reference=format!("refs/heads/{branch}");let new=reference_object(git,&reference)?;
            losses.push(Loss{path:None,reference:Some(format!("{remote}/{reference}")),description:format!("Replace observed remote tip {old} with local tip {new}; the lease rejects any remote tip change")});
            commit_losses(git,old,Some(&new),None,losses)?;
            hints.push(format!("Observed remote history: git show {old}; preserve it locally with git branch recovery-{short} {old}",short=&old[..12]));
        },
        Action::StashDrop{oid}|Action::StashApply{oid,pop:true}=>{
            losses.push(Loss{path:None,reference:Some(oid.clone()),description:format!("Remove stash entry for exact object {oid}")});
            hints.push(format!("Recover the dropped stash while its object exists: git stash apply {oid}"));
        },
        _=>{losses.push(Loss{path:None,reference:None,description:"Git may update the index, working tree, and/or current branch history".into()});}
    }
    Ok(())
}

pub fn preview(repo: Repo, action: Action) -> Result<Plan> {
    let repo=repo.refreshed_head()?;
    let mut git = Git::new(repo);
    let mut observed_lease = None;
    if let Action::Push {
        remote,
        branch,
        force_with_lease: true,
    } = &action
    {
        super::remote(&mut git, remote)?;
        super::reference(&mut git, branch, true)?;
        let data = git.run(
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &format!("refs/remotes/{remote}/{branch}^{{commit}}"),
            ],
            None,
        )?;
        ensure!(
            data.status.success(),
            "Fetch this remote branch before previewing a force push; an observed remote-tracking tip is required"
        );
        observed_lease = Some(text(&data.stdout).trim().into());
    }
    let _ = super::arguments(&mut git, &action, observed_lease.as_deref())?;
    let paths = selected_paths(&mut git, &action)?;
    let fingerprint = fingerprint(&mut git, &paths)?;
    let destructive = destructive(&action);
    let config = Config::load(&git.repo.store)?;
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in &config.secrets_denylist {
        builder.add(
            globset::GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()?,
        );
    }
    let denied = builder.build()?;
    let mut safety = Vec::new();
    let mut skipped = Vec::new();
    let mut losses = Vec::new();
    let mut reflog_hints=vec!["git reflog --date=iso; paradox loops <file>".into()];
    if destructive {
        let sources:&[&str]=match &action{
            Action::BranchDelete{..}|Action::TagDelete{..}|Action::Push{..}|Action::StashDrop{..}|Action::Reset{mode:ResetMode::Soft,..}=>&[],
            Action::Reset{mode:ResetMode::Mixed,..}=>&["staged"],
            _=>&["working","staged"],
        };
        for path in &paths {
            git.path(path)?;
            if sources.is_empty(){continue;}
            losses.push(Loss {
                path: Some(path.clone()),
                reference: None,
                description: "Working or staged content may be replaced by this Git operation"
                    .into(),
            });
            for &source in sources {
                let reason =
                    if denied.is_match(path) || path.split('/').any(|part| denied.is_match(part)) {
                        Some("secret denylist: content will not enter loop storage".to_owned())
                    } else {
                        match snapshot(&mut git, path, source) {
                            Ok((bytes, _, _)) if bytes.len() as u64 > config.size_limit_bytes => {
                                Some("file exceeds configured loop size limit".into())
                            }
                            Ok((bytes, _, _)) if bytes.iter().take(8192).any(|b| *b == 0) => {
                                Some("binary content is not protected by text safety loops".into())
                            }
                            Ok(_) => None,
                            Err(error) => Some(error.to_string()),
                        }
                    };
                let entry = Protection {
                    path: path.clone(),
                    source: source.into(),
                    reason,
                };
                if entry.reason.is_some() {
                    skipped.push(entry);
                } else {
                    safety.push(entry);
                }
            }
        }
        exact_losses(&mut git,&action,observed_lease.as_deref(),&mut losses,&mut reflog_hints)?;
    }
    Ok(Plan {
        action,
        fingerprint,
        destructive,
        losses,
        safety,
        skipped,
        reflog_hints,
        commands: git.commands,
        observed_lease,
        paths,
    })
}
pub(super) fn validate(repo: &Repo, plan: &Plan) -> Result<()> {
    let mut git = Git::new(repo.clone());
    let current = selected_paths(&mut git, &plan.action)?;
    if current != plan.paths || fingerprint(&mut git, &current)? != plan.fingerprint {
        return Err(ActionError {
            code: "stale_plan",
            message: "Repository changed since preview; review a new plan".into(),
            commands: git.commands,
        }
        .into());
    }
    Ok(())
}
pub(super) fn protect(repo: &Repo, plan: &Plan) -> Result<Vec<Value>> {
    let mut store = LoopStore::open(repo.clone().refreshed_head()?)?;
    validate(repo, plan)?;
    let mut git = Git::new(repo.clone());
    let mut protected = Vec::new();
    for entry in &plan.safety {
        let (bytes, deleted, mode) = snapshot(&mut git, &entry.path, &entry.source)?;
        ensure!(
            !store.denied(&entry.path),
            "safety policy changed since preview"
        );
        ensure!(
            bytes.len() as u64 <= store.config.size_limit_bytes,
            "safety size policy changed"
        );
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let message = format!("before UI action ({})", entry.source);
        let id = store.save_loop(NewLoop {
            path: &entry.path,
            bytes: &bytes,
            hash: &hash,
            state: None,
            kind: "safety",
            message: &message,
            auto_label: false,
            deleted,
            pinned: true,
            mode,
        })?;
        // Other agents may open this store while Git runs. A live UI operation owns
        // this durable pin, rather than the old single-lock crash-recovery pins.
        store
            .index
            .execute("DELETE FROM operation_pins WHERE id=?1", [id])?;
        protected.push(json!({"path":entry.path,"source":entry.source,"id":id}));
    }
    Ok(protected)
}
pub(super) fn release(repo: &Repo, loops: &[Value]) -> Result<()> {
    if loops.is_empty() {
        return Ok(());
    }
    let store = LoopStore::open(repo.clone())?;
    for row in loops {
        if let Some(id) = row["id"].as_i64() {
            store.pin_loop(id, false)?;
        }
    }
    Ok(())
}
pub(super) struct ProtectionGuard {
    pub repo: Repo,
    pub loops: Vec<Value>,
}
impl Drop for ProtectionGuard {
    fn drop(&mut self) {
        if let Err(error) = release(&self.repo, &self.loops) {
            crate::store::log_at(
                &self.repo.store,
                &format!("UI safety pins retained after operation: {error:#}"),
            );
        }
    }
}
pub(super) fn discard(repo: &Repo, paths: &[String]) -> Result<Vec<crate::commit::CommandRecord>> {
    let current=repo.clone().refreshed_head()?;
    let repo=&current;
    let mut git = Git::new(repo.clone());
    let head = git.head()?;
    let mut store = LoopStore::open(repo.clone())?;
    let expanded = selected_paths(
        &mut git,
        &Action::Discard {
            paths: paths.to_vec(),
        },
    )?;
    for path in expanded {
        let target = head.as_ref().map(|head| format!("{head}:{path}"));
        let exists = if let Some(target) = &target {
            git.run(&["cat-file", "-e", target], None)?.status.success()
        } else {
            false
        };
        if exists {
            let version = store.head_version(&path)?;
            store.apply_rewinds(&[crate::history::RewindPlan {
                path: path.clone(),
                version,
            }])?;
            git.checked(&["--literal-pathspecs","restore","--staged","--source=HEAD","--",&path],None)?;
        } else {
            write_result(&mut store, &path, None)?;
            git.checked(&["--literal-pathspecs","rm","--cached","-f","--ignore-unmatch","--",&path],None)?;
        }
    }
    Ok(git.commands)
}
pub(super) fn write_result(store: &mut LoopStore, path: &str, bytes: Option<&[u8]>) -> Result<()> {
    let target = store.repo.root.join(path);
    ensure!(store.repo.relative(&target)? == path, "invalid path");
    // Revalidate every ancestor without following links before a native write.
    let mut current = store.repo.root.clone();
    for part in std::path::Path::new(path).components() {
        ensure!(
            matches!(part, std::path::Component::Normal(_)),
            "invalid path"
        );
        current.push(part.as_os_str());
        if let Ok(metadata) = fs::symlink_metadata(&current) {
            ensure!(
                !metadata.file_type().is_symlink(),
                "refusing a linked write target"
            );
        }
    }
    let hash = blake3::hash(bytes.unwrap_or_default()).to_hex().to_string();
    store.writing_marker(path, &hash, bytes.is_none())?;
    if let Some(bytes) = bytes {
        crate::blob::atomic_write(&target, bytes)?;
        let metadata = store
            .repo
            .regular_file(path)?
            .context("write target changed type")?;
        store.cache_write(path, crate::store::stamp(&metadata)?, &hash)?;
    } else {
        match fs::remove_file(&target) {
            Ok(()) => {
                #[cfg(unix)]
                fs::File::open(target.parent().context("missing parent")?)?.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        };
        store.index.execute(
            "DELETE FROM stat_cache WHERE worktree=?1 AND path=?2",
            rusqlite::params![store.repo.worktree, path],
        )?;
    }
    store.remove_writing_marker()?;
    Ok(())
}
