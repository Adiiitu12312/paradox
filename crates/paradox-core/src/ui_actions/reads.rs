use super::{fields, text};
use crate::{repo::Repo, ui_git::Git};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub fn state(repo: Repo) -> Result<Value> {
    let mut git = Git::new(repo);
    let head = git.head()?;
    let unmerged = if head.is_some() {
        git.checked(
            &[
                "for-each-ref",
                "--no-merged=HEAD",
                "--format=%(refname)",
                "refs/heads/",
            ],
            None,
        )?
    } else {
        Vec::new()
    };
    let unmerged = text(&unmerged);
    let refs = git.checked(
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(*objectname)%00%(HEAD)%00%(upstream:short)",
            "refs/heads/",
            "refs/tags/",
        ],
        None,
    )?;
    let mut branches = Vec::new();
    let mut tags = Vec::new();
    for row in refs.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        let parts: Vec<_> = row.split(|b| *b == 0).collect();
        ensure!(parts.len() == 5, "invalid native ref record");
        let name = text(parts[0]);
        if let Some(name) = name.strip_prefix("refs/heads/") {
            branches.push(json!({"name":name,"sha":text(parts[1]),"current":parts[3]==b"*","upstream":(!parts[4].is_empty()).then(||text(parts[4])),"unmerged":unmerged.lines().any(|line|line==format!("refs/heads/{name}"))}));
        } else if let Some(name) = name.strip_prefix("refs/tags/") {
            tags.push(
                json!({"name":name,"sha":text(if parts[2].is_empty(){parts[1]}else{parts[2]})}),
            );
        }
    }
    let stash = git.checked(&["stash", "list", "-z", "--format=%H%x00%gd%x00%gs"], None)?;
    let stash = fields(&stash);
    ensure!(
        stash.len().is_multiple_of(3),
        "invalid native stash records"
    );
    let stashes: Vec<_> = stash
        .as_chunks::<3>()
        .0
        .iter()
        .map(|row| json!({"oid":text(row[0]),"selector":text(row[1]),"subject":text(row[2])}))
        .collect();
    let names = git.checked(&["remote"], None)?;
    let mut remotes = Vec::new();
    for name in text(&names).lines() {
        let fetch = git.checked(&["remote", "get-url", name], None)?;
        let push = git.checked(&["remote", "get-url", "--push", name], None)?;
        remotes.push(json!({"name":name,"fetch_url":text(&fetch).trim_end(),"push_url":text(&push).trim_end()}));
    }
    let root = &git.repo.git_dir;
    let operation = if root.join("rebase-merge").is_dir() || root.join("rebase-apply").is_dir() {
        Some("rebase")
    } else if root.join("MERGE_HEAD").exists() {
        Some("merge")
    } else if root.join("CHERRY_PICK_HEAD").exists() {
        Some("cherry_pick")
    } else if root.join("REVERT_HEAD").exists() {
        Some("revert")
    } else {
        None
    };
    Ok(git.finish(json!({"branches":branches,"tags":tags,"stashes":stashes,"remotes":remotes,"operation":operation})))
}

pub fn history(repo: Repo, path: &str, cursor: Option<&str>) -> Result<Value> {
    let mut git = Git::new(repo);
    let path = git.path(path)?;
    let skip: u32 = cursor
        .unwrap_or("0")
        .parse()
        .context("invalid history cursor")?;
    let data = git.checked(
        &[
            "--literal-pathspecs",
            "log",
            "--follow",
            "--no-show-signature",
            "--no-notes",
            "-z",
            "--format=%H%x00%P%x00%an%x00%ae%x00%at%x00%s",
            "--max-count=201",
            &format!("--skip={skip}"),
            "--",
            &path,
        ],
        None,
    )?;
    let records: Vec<_> = data.split(|b| *b == 0).collect();
    let records = if records.last() == Some(&b"".as_slice()) {
        &records[..records.len() - 1]
    } else {
        &records
    };
    ensure!(records.len().is_multiple_of(6), "invalid history records");
    let mut commits:Vec<_>=records.as_chunks::<6>().0.iter().map(|row|Ok(json!({"sha":text(row[0]),"parents":text(row[1]).split_whitespace().collect::<Vec<_>>(),"author_name":text(row[2]),"author_email":text(row[3]),"time":std::str::from_utf8(row[4])?.parse::<i64>()?,"subject":text(row[5]),"refs":[]}))).collect::<Result<_>>()?;
    let more = commits.len() > 200;
    commits.truncate(200);
    Ok(git.finish(
        json!({"commits":commits,"next_cursor":more.then(||skip.saturating_add(200).to_string())}),
    ))
}

pub fn blame(repo: Repo, path: &str, revision: Option<&str>) -> Result<Value> {
    let mut git = Git::new(repo);
    let path = git.path(path)?;
    let resolved = revision
        .map(|rev| super::revision(&mut git, rev))
        .transpose()?;
    let mut args = vec!["--literal-pathspecs", "blame", "--line-porcelain"];
    if let Some(rev) = &resolved {
        args.push(rev);
    }
    args.extend(["--", &path]);
    let output = git.checked(&args, None)?;
    let mut sha = String::new();
    let mut original = 0;
    let mut line = 0;
    let mut author = String::new();
    let mut email = String::new();
    let mut time = 0;
    let mut summary = String::new();
    let mut lines = Vec::new();
    for row in output.split_inclusive(|b| *b == b'\n') {
        if row.starts_with(b"\t") {
            lines.push(json!({"line":line,"original_line":original,"sha":sha,"author_name":author,"author_email":email,"time":time,"summary":summary,"text":text(&row[1..])}));
            continue;
        }
        let value = text(row);
        let value = value.trim_end_matches('\n');
        if let Some(value) = value.strip_prefix("author ") {
            author = value.into();
        } else if let Some(value) = value.strip_prefix("author-mail ") {
            email = value.trim_matches(['<', '>']).into();
        } else if let Some(value) = value.strip_prefix("author-time ") {
            time = value.parse::<i64>()?;
        } else if let Some(value) = value.strip_prefix("summary ") {
            summary = value.into();
        } else {
            let parts: Vec<_> = value.split(' ').collect();
            if parts.len() >= 3
                && matches!(parts[0].len(), 40 | 64)
                && parts[0].bytes().all(|b| b.is_ascii_hexdigit())
            {
                sha = parts[0].into();
                original = parts[1].parse::<u64>()?;
                line = parts[2].parse::<u64>()?;
            }
        }
    }
    Ok(git.finish(json!({"path":path,"revision":resolved,"lines":lines})))
}
