//! Typed browser operations. Git remains the only Git implementation.
mod conflict;
mod process;
mod reads;
mod safety;
use crate::{
    repo::Repo,
    ui_git::{ActionError, Git},
};
use anyhow::{Context, Result, bail, ensure};
pub use conflict::{ResolveRequest, conflict, resolve_conflict};
pub use process::{Cancellation, ProgressEvent};
pub use reads::{blame, history, state};
pub use safety::{Plan, preview};
pub fn validate_plan(repo: &Repo, plan: &Plan) -> Result<()> {
    safety::validate(repo, plan)
}
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    BranchCreate {
        name: String,
        start: Option<String>,
    },
    BranchSwitch {
        name: String,
    },
    BranchRename {
        name: String,
        new_name: String,
    },
    BranchDelete {
        name: String,
        #[serde(default)]
        force: bool,
    },
    Merge {
        revision: String,
    },
    Rebase {
        onto: String,
        #[serde(default)]
        interactive: bool,
    },
    CherryPick {
        revision: String,
    },
    Revert {
        revision: String,
    },
    TagCreate {
        name: String,
        target: Option<String>,
        message: Option<String>,
    },
    TagDelete {
        name: String,
    },
    StashCreate {
        message: Option<String>,
        #[serde(default)]
        include_untracked: bool,
    },
    StashApply {
        oid: String,
        #[serde(default)]
        pop: bool,
    },
    StashDrop {
        oid: String,
    },
    Fetch {
        remote: String,
        #[serde(default)]
        prune: bool,
    },
    Pull {
        remote: String,
        branch: String,
        #[serde(default)]
        rebase: bool,
    },
    Push {
        remote: String,
        branch: String,
        #[serde(default)]
        force_with_lease: bool,
    },
    Reset {
        target: String,
        mode: ResetMode,
    },
    Discard {
        paths: Vec<String>,
    },
    Continue {
        operation: Operation,
    },
    Abort {
        operation: Operation,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResetMode {
    Soft,
    Mixed,
    Hard,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

fn fields(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .collect()
}
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
fn reference(git: &mut Git, name: &str, branch: bool) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.starts_with('-') && !name.contains('\0'),
        "invalid reference name"
    );
    let full = if branch {
        format!("refs/heads/{name}")
    } else {
        format!("refs/tags/{name}")
    };
    git.checked(&["check-ref-format", &full], None)?;
    Ok(())
}
fn revision(git: &mut Git, name: &str) -> Result<String> {
    ensure!(!name.is_empty() && !name.contains('\0'), "invalid revision");
    let value = git.checked(
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{name}^{{commit}}"),
        ],
        None,
    )?;
    Ok(text(&value).trim().into())
}
fn remote(git: &mut Git, name: &str) -> Result<()> {
    ensure!(
        !name.starts_with('-') && !name.contains('\0'),
        "invalid remote name"
    );
    let names = git.checked(&["remote"], None)?;
    ensure!(
        text(&names).lines().any(|line| line == name),
        "unknown remote"
    );
    Ok(())
}
fn stash_selector(git: &mut Git, oid: &str) -> Result<String> {
    ensure!(
        oid.len() == 40 || oid.len() == 64,
        "invalid stash object ID"
    );
    ensure!(
        oid.bytes().all(|c| c.is_ascii_hexdigit()),
        "invalid stash object ID"
    );
    let data = git.checked(&["stash", "list", "-z", "--format=%H%x00%gd"], None)?;
    let records: Vec<_> = data.split(|b| *b == 0).collect();
    for pair in records.as_chunks::<2>().0.iter() {
        if pair[0] == oid.as_bytes() {
            return Ok(text(pair[1]));
        }
    }
    bail!("stash no longer exists")
}
fn arguments(git: &mut Git, action: &Action, lease: Option<&str>) -> Result<Vec<String>> {
    let args: Vec<String> = match action {
        Action::BranchCreate { name, start } => {
            reference(git, name, true)?;
            let mut a = vec!["branch".into(), name.clone()];
            if let Some(start) = start {
                a.push(revision(git, start)?);
            }
            a
        }
        Action::BranchSwitch { name } => {
            reference(git, name, true)?;
            vec!["switch".into(), name.clone()]
        }
        Action::BranchRename { name, new_name } => {
            reference(git, name, true)?;
            reference(git, new_name, true)?;
            vec!["branch".into(), "-m".into(), name.clone(), new_name.clone()]
        }
        Action::BranchDelete { name, force } => {
            reference(git, name, true)?;
            vec![
                "branch".into(),
                if *force { "-D" } else { "-d" }.into(),
                name.clone(),
            ]
        }
        Action::Merge { revision: rev } => {
            vec!["merge".into(), "--no-edit".into(), revision(git, rev)?]
        }
        Action::Rebase { onto, interactive } => {
            ensure!(
                !interactive,
                "Interactive rebase is available in the terminal: paradox rebase -i <upstream>"
            );
            vec!["rebase".into(), revision(git, onto)?]
        }
        Action::CherryPick { revision: rev } => vec!["cherry-pick".into(), revision(git, rev)?],
        Action::Revert { revision: rev } => {
            vec!["revert".into(), "--no-edit".into(), revision(git, rev)?]
        }
        Action::TagCreate {
            name,
            target,
            message,
        } => {
            reference(git, name, false)?;
            let mut a = vec!["tag".into()];
            if let Some(message) = message {
                a.extend(["-a".into(), "-m".into(), message.clone()]);
            }
            a.push(name.clone());
            if let Some(target) = target {
                a.push(revision(git, target)?);
            }
            a
        }
        Action::TagDelete { name } => {
            reference(git, name, false)?;
            vec!["tag".into(), "-d".into(), name.clone()]
        }
        Action::StashCreate {
            message,
            include_untracked,
        } => {
            let mut a = vec!["stash".into(), "push".into()];
            if *include_untracked {
                a.push("--include-untracked".into());
            }
            if let Some(message) = message {
                a.extend(["-m".into(), message.clone()]);
            }
            a
        }
        Action::StashApply { oid, pop } => vec![
            "stash".into(),
            if *pop { "pop" } else { "apply" }.into(),
            stash_selector(git, oid)?,
        ],
        Action::StashDrop { oid } => vec!["stash".into(), "drop".into(), stash_selector(git, oid)?],
        Action::Fetch {
            remote: name,
            prune,
        } => {
            remote(git, name)?;
            let mut a = vec!["fetch".into(), "--progress".into()];
            if *prune {
                a.push("--prune".into());
            }
            a.push(name.clone());
            a
        }
        Action::Pull {
            remote: name,
            branch,
            rebase,
        } => {
            remote(git, name)?;
            reference(git, branch, true)?;
            vec![
                "pull".into(),
                "--progress".into(),
                if *rebase { "--rebase" } else { "--no-rebase" }.into(),
                "--no-edit".into(),
                name.clone(),
                branch.clone(),
            ]
        }
        Action::Push {
            remote: name,
            branch,
            force_with_lease,
        } => {
            remote(git, name)?;
            reference(git, branch, true)?;
            let mut a = vec!["push".into(), "--progress".into()];
            if *force_with_lease {
                a.push(format!(
                    "--force-with-lease=refs/heads/{branch}:{}",
                    lease.context("Force push requires a fresh observed-tip confirmation")?
                ));
            }
            a.extend([
                name.clone(),
                format!("refs/heads/{branch}:refs/heads/{branch}"),
            ]);
            a
        }
        Action::Reset { target, mode } => vec![
            "reset".into(),
            match mode {
                ResetMode::Soft => "--soft",
                ResetMode::Mixed => "--mixed",
                ResetMode::Hard => "--hard",
            }
            .into(),
            revision(git, target)?,
            "--".into(),
        ],
        Action::Discard { .. } => Vec::new(),
        Action::Continue { operation } | Action::Abort { operation } => {
            let abort = matches!(action, Action::Abort { .. });
            let command = match operation {
                Operation::Merge => "merge",
                Operation::Rebase => "rebase",
                Operation::CherryPick => "cherry-pick",
                Operation::Revert => "revert",
            };
            vec![
                command.into(),
                if abort { "--abort" } else { "--continue" }.into(),
            ]
        }
    };
    Ok(args)
}

pub fn execute(
    repo: Repo,
    action: Action,
    plan: Option<&Plan>,
    cancel: Cancellation,
    mut on_event: impl FnMut(ProgressEvent) + Send,
) -> Result<Value> {
    let repo=repo.refreshed_head()?;
    let destructive = safety::destructive(&action);
    if destructive {
        let plan = plan.context("This action requires preview and explicit confirmation")?;
        ensure!(plan.action == action, "confirmation action mismatch");
        safety::validate(&repo, plan)?;
    }
    let mut git = Git::new(repo.clone());
    let lease = plan.and_then(|plan| plan.observed_lease.as_deref());
    let args = arguments(&mut git, &action, lease)?;
    let safety_loops = if destructive {
        safety::protect(&repo, plan.expect("checked confirmation"))?
    } else {
        Vec::new()
    };
    let _safety = safety::ProtectionGuard {
        repo: repo.clone(),
        loops: safety_loops.clone(),
    };
    if let Some(plan) = plan && destructive {
        safety::validate(&repo, plan)?;
    }
    if let Action::Discard { paths } = &action {
        git.commands.extend(safety::discard(&repo, paths)?);
        return Ok(git.finish(json!({"changed":true,"safety_loops":safety_loops})));
    }
    let argv: Vec<_> = args.iter().map(String::as_str).collect();
    let mut command = repo.command(&argv);
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no");
    let record = process::run(command, cancel, &mut on_event)?;
    let code = record.exit_code;
    git.commands.push(record);
    if code != 0 {
        return Err(ActionError {
            code: "git_failed",
            message: format!("Git operation exited with {code}; see the command log"),
            commands: git.commands,
        }
        .into());
    }
    Ok(git.finish(json!({"changed":true,"safety_loops":safety_loops})))
}
