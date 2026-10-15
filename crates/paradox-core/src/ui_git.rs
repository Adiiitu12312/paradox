//! Bounded native Git operations shared by the browser API.
use crate::{commit::CommandRecord, repo::Repo};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashMap, fs, io::Read, path::{Component, Path}};

const LIMIT: usize = 8_388_608;

#[derive(Debug)]
pub struct ActionError {
    pub code: &'static str,
    pub message: String,
    pub commands: Vec<CommandRecord>,
}
impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.message) }
}
impl std::error::Error for ActionError {}

struct Git {
    repo: Repo,
    commands: Vec<CommandRecord>,
}
impl Git {
    fn new(repo: Repo) -> Self { Self { repo, commands: Vec::new() } }
    fn run(&mut self, args: &[&str], input: Option<&[u8]>) -> Result<std::process::Output> {
        let command = self.repo.command(args);
        let argv = std::iter::once(command.get_program()).chain(command.get_args())
            .map(|arg| arg.to_string_lossy().into_owned()).collect();
        let output = crate::git::output_bounded(command, input, LIMIT)?;
        self.commands.push(CommandRecord { argv, exit_code: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout[..output.stdout.len().min(65_536)]).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(65_536)]).into_owned() });
        Ok(output)
    }
    fn checked(&mut self, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
        let output = self.run(args, input)?;
        if !output.status.success() {
            return Err(ActionError { code: "git_failed", message: String::from_utf8_lossy(&output.stderr).into_owned(), commands: self.commands.clone() }.into());
        }
        Ok(output.stdout)
    }
    fn head(&mut self) -> Result<Option<String>> {
        let output = self.run(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"], None)?;
        if output.status.code() == Some(1) { return Ok(None); }
        ensure!(output.status.success(), "Git HEAD lookup failed");
        Ok(Some(std::str::from_utf8(&output.stdout)?.trim().to_owned()))
    }
    fn finish(self, mut value: Value) -> Value {
        value["commands"] = json!(self.commands);
        value
    }
    fn path(&self, path: &str) -> Result<String> {
        let path = path.trim_end_matches('/');
        let normalized = self.repo.relative(&self.repo.root.join(path))?;
        ensure!(normalized == path, "path must be repository-root-relative");
        ensure!(!Path::new(path).components().any(|part| !matches!(part, Component::Normal(_))), "invalid path");
        ensure!(!path.split('/').any(|part| matches!(part, ".git" | ".paradox")), "repository metadata cannot be edited through the UI");
        Ok(normalized)
    }
}

fn records(bytes: &[u8]) -> Vec<&[u8]> {
    let mut fields: Vec<_> = bytes.split(|byte| *byte == 0).collect();
    if fields.last() == Some(&b"".as_slice()) { fields.pop(); }
    fields
}
fn text(bytes: &[u8]) -> String { String::from_utf8_lossy(bytes).into_owned() }
fn valid_sha(sha: &str) -> Result<()> {
    ensure!(matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit()), "invalid commit ID");
    Ok(())
}

pub fn status(repo: Repo) -> Result<Value> {
    let mut git = Git::new(repo);
    let bytes = git.checked(&["status", "--porcelain=v2", "-z", "--untracked-files=normal"], None)?;
    let fields = records(&bytes);
    let mut files = Vec::new();
    let mut index = 0;
    while let Some(entry) = fields.get(index) {
        index += 1;
        if entry.starts_with(b"? ") {
            files.push(json!({"path": std::str::from_utf8(&entry[2..])?, "old_path": null, "index_status": "?", "worktree_status": "?", "untracked": true}));
            continue;
        }
        let count = match entry.first() { Some(b'1') => 9, Some(b'2') => 10, Some(b'u') => 11, _ => continue };
        let parts: Vec<_> = entry.splitn(count, |byte| *byte == b' ').collect();
        ensure!(parts.len() == count && parts[1].len() == 2, "invalid Git status record");
        let old = if entry[0] == b'2' {
            let old = fields.get(index).context("missing rename source")?; index += 1;
            Some(std::str::from_utf8(old)?)
        } else { None };
        files.push(json!({"path": std::str::from_utf8(parts[count-1])?, "old_path": old,
            "index_status": text(&parts[1][..1]), "worktree_status": text(&parts[1][1..]), "untracked": false}));
    }
    let head = git.head()?;
    let branch = git.run(&["symbolic-ref", "--quiet", "--short", "HEAD"], None)?;
    let branch = branch.status.success().then(|| text(&branch.stdout).trim().to_owned());
    let value = json!({"repo": {"root": git.repo.root.display().to_string(), "name": git.repo.root.file_name().unwrap_or_default().to_string_lossy(), "branch": branch, "unborn": head.is_none(), "head": head}, "files": files});
    Ok(git.finish(value))
}

fn commit_rows(bytes: &[u8]) -> Result<Vec<Value>> {
    let fields = records(bytes);
    ensure!(fields.len().is_multiple_of(6), "invalid native Git graph records");
    fields.chunks_exact(6).map(|row| Ok(json!({"sha": std::str::from_utf8(row[0])?,
        "parents": std::str::from_utf8(row[1])?.split_whitespace().collect::<Vec<_>>(),
        "author_name": text(row[2]), "author_email": text(row[3]), "time": std::str::from_utf8(row[4])?.parse::<i64>()?,
        "subject": text(row[5]), "refs": []}))).collect()
}

pub fn graph(repo: Repo, cursor: Option<&str>) -> Result<Value> {
    let skip: u32 = cursor.unwrap_or("0").parse().context("invalid graph cursor")?;
    let mut git = Git::new(repo);
    let head = git.head()?;
    let mut commits = commit_rows(&git.checked(&["log", "--all", "--date-order", "--no-show-signature", "--no-notes", "--no-decorate", "-z",
            "--format=%H%x00%P%x00%an%x00%ae%x00%at%x00%s", "--max-count=201", &format!("--skip={skip}")], None)?)?;
    let more = commits.len() > 200;
    commits.truncate(200);
    let refs = git.checked(&["for-each-ref", "--format=%(objectname)%00%(*objectname)%00%(refname)"], None)?;
    let mut references: HashMap<String, Vec<String>> = HashMap::new();
    for line in refs.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
        let fields: Vec<_> = line.split(|byte| *byte == 0).collect();
        ensure!(fields.len() == 3, "invalid Git ref record");
        let sha = if fields[1].is_empty() { fields[0] } else { fields[1] };
        references.entry(text(sha)).or_default().push(text(fields[2]));
    }
    for commit in &mut commits { commit["refs"] = json!(references.get(commit["sha"].as_str().unwrap_or_default()).cloned().unwrap_or_default()); }
    Ok(git.finish(json!({"commits": commits, "next_cursor": more.then(|| skip.saturating_add(200).to_string()), "head": head})))
}

pub fn commit_detail(repo: Repo, sha: &str) -> Result<Value> {
    valid_sha(sha)?;
    let mut git = Git::new(repo);
    let bytes = git.checked(&["show", "-s", "--no-show-signature", "--no-notes", "-z", "--format=%H%x00%P%x00%an%x00%ae%x00%at%x00%s%x00%B", sha, "--"], None)?;
    let fields = records(&bytes);
    ensure!(fields.len() == 7, "invalid commit metadata");
    let mut value = json!({"sha": text(fields[0]), "parents": text(fields[1]).split_whitespace().collect::<Vec<_>>(),
        "author_name": text(fields[2]), "author_email": text(fields[3]), "time": std::str::from_utf8(fields[4])?.parse::<i64>()?,
        "subject": text(fields[5]), "message": text(fields[6])});
    let names = git.checked(&["diff-tree", "--root", "--diff-merges=first-parent", "-r", "--no-commit-id", "--name-status", "-z", "--find-renames", sha, "--"], None)?;
    let stats = git.checked(&["diff-tree", "--root", "--diff-merges=first-parent", "-r", "--no-commit-id", "--numstat", "-z", "--find-renames", sha, "--"], None)?;
    let stats = records(&stats);
    let mut sizes = HashMap::new(); let mut index = 0;
    while let Some(record) = stats.get(index) {
        index += 1;
        let parts: Vec<_> = record.splitn(3, |byte| *byte == b'\t').collect();
        ensure!(parts.len() == 3, "invalid numstat record");
        let path = if parts[2].is_empty() { index += 2; *stats.get(index-1).context("missing renamed numstat path")? } else { parts[2] };
        sizes.insert(text(path), (std::str::from_utf8(parts[0])?.parse::<u64>().ok(), std::str::from_utf8(parts[1])?.parse::<u64>().ok()));
    }
    let names = records(&names); let mut files = Vec::new(); let mut index = 0;
    while let Some(status) = names.get(index) {
        index += 1;
        let first = *names.get(index).context("missing changed path")?; index += 1;
        let (old, path) = if matches!(status.first(), Some(b'R' | b'C')) { let path = *names.get(index).context("missing rename path")?; index += 1; (Some(text(first)), text(path)) } else { (None, text(first)) };
        let (additions, deletions) = sizes.get(&path).copied().unwrap_or_default();
        files.push(json!({"path": path, "old_path": old, "status": text(status), "additions": additions, "deletions": deletions}));
    }
    value["files"] = json!(files);
    Ok(git.finish(value))
}

#[derive(Clone)]
struct Hunk { header: String, lines: Vec<String>, old_start: usize, new_start: usize }
fn range(value: &str) -> Result<usize> { Ok(value[1..].split(',').next().context("missing hunk range")?.parse()?) }
fn hunks(patch: &str) -> Result<(String, Vec<Hunk>)> {
    let mut prefix = String::new(); let mut result: Vec<Hunk> = Vec::new();
    for line in patch.split_inclusive('\n') {
        if line.starts_with("@@ ") {
            let fields: Vec<_> = line.splitn(4, ' ').collect();
            ensure!(fields.len() == 4, "invalid native hunk header");
            result.push(Hunk { header: line.to_owned(), lines: Vec::new(), old_start: range(fields[1])?, new_start: range(fields[2])? });
        } else if let Some(hunk) = result.last_mut() { hunk.lines.push(line.to_owned()); }
        else { prefix.push_str(line); }
    }
    Ok((prefix, result))
}

fn worktree(repo: &Repo, path: &str) -> Result<(Vec<u8>, bool, bool)> {
    let mut absolute = repo.root.clone();
    let parts: Vec<_> = Path::new(path).components().collect();
    for (index, part) in parts.iter().enumerate() {
        absolute.push(part.as_os_str());
        let metadata = match fs::symlink_metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), false, false)),
            Err(error) => return Err(error.into()),
        };
        if index + 1 != parts.len() { ensure!(!metadata.file_type().is_symlink() && metadata.is_dir(), "unsafe path parent"); }
        else if metadata.file_type().is_symlink() { return Ok((fs::read_link(&absolute)?.as_os_str().as_encoded_bytes().to_vec(), true, false)); }
        else { ensure!(metadata.is_file(), "diff requires a file"); }
    }
    let mut bytes = Vec::new(); fs::File::open(absolute)?.take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    let truncated = bytes.len() > LIMIT; bytes.truncate(LIMIT);
    Ok((bytes, true, truncated))
}

fn blob(git: &mut Git, spec: &str) -> Result<(Vec<u8>, bool, bool)> {
    let exists = git.run(&["cat-file", "-e", spec], None)?;
    if !exists.status.success() { return Ok((Vec::new(), false, false)); }
    let size = git.checked(&["cat-file", "-s", spec], None)?;
    let size: usize = std::str::from_utf8(&size)?.trim().parse()?;
    if size > LIMIT { return Ok((Vec::new(), true, true)); }
    Ok((git.checked(&["cat-file", "blob", spec], None)?, true, false))
}

struct Diff { value: Value, prefix: String, hunks: Vec<Hunk> }
fn diff_inner(git: &mut Git, path: &str, view: &str, commit: Option<&str>) -> Result<Diff> {
    let path = git.path(path)?;
    ensure!(matches!(view, "working" | "staged"), "invalid diff view");
    let head = git.head()?;
    let (old, new, patch) = if let Some(sha) = commit {
        valid_sha(sha)?;
        let parents = git.checked(&["show", "-s", "--format=%P", sha], None)?;
        let parent = std::str::from_utf8(&parents)?.split_whitespace().next();
        let names = git.checked(&["diff-tree", "--root", "--diff-merges=first-parent", "-r", "--no-commit-id", "--name-status", "-z", "--find-renames", sha, "--"], None)?;
        let names = records(&names); let mut index = 0; let mut old_path = path.as_str();
        while let Some(status) = names.get(index) {
            index += 1; let first = *names.get(index).context("missing commit path")?; index += 1;
            if matches!(status.first(), Some(b'R' | b'C')) {
                let next = *names.get(index).context("missing renamed commit path")?; index += 1;
                if next == path.as_bytes() { old_path = std::str::from_utf8(first)?; }
            }
        }
        let old = if let Some(parent) = parent { blob(git, &format!("{parent}:{old_path}"))? } else { (Vec::new(), false, false) };
        let new = blob(git, &format!("{sha}:{path}"))?;
        (old, new, Vec::new())
    } else if view == "staged" {
        let old = if let Some(head) = &head { blob(git, &format!("{head}:{path}"))? } else { (Vec::new(), false, false) };
        let new = blob(git, &format!(":0:{path}"))?;
        let patch = if old.2 || new.2 { Vec::new() } else { git.checked(&["--literal-pathspecs", "diff", "--cached", "--no-ext-diff", "--no-textconv", "--no-color", "--no-renames", "--unified=3", "--src-prefix=a/", "--dst-prefix=b/", "--", &path], None)? };
        (old, new, patch)
    } else {
        let old = blob(git, &format!(":0:{path}"))?;
        let new = worktree(&git.repo, &path)?;
        let patch = if old.2 || new.2 { Vec::new() }
            else if old.1 { git.checked(&["--literal-pathspecs", "diff", "--no-ext-diff", "--no-textconv", "--no-color", "--no-renames", "--unified=3", "--src-prefix=a/", "--dst-prefix=b/", "--", &path], None)? }
            else if new.1 {
                let output = git.run(&["diff", "--no-index", "--no-ext-diff", "--no-textconv", "--no-color", "--unified=3", "--src-prefix=a/", "--dst-prefix=b/", "--", "/dev/null", &path], None)?;
                ensure!(matches!(output.status.code(), Some(0 | 1)), "could not diff new file"); output.stdout
            } else { Vec::new() };
        (old, new, patch)
    };
    let binary = old.0.contains(&0) || new.0.contains(&0) || std::str::from_utf8(&old.0).is_err() || std::str::from_utf8(&new.0).is_err();
    let truncated = old.2 || new.2;
    let mut hasher = blake3::Hasher::new();
    for bytes in [path.as_bytes(), view.as_bytes(), head.as_deref().unwrap_or_default().as_bytes(), &old.0, &new.0, &patch] { hasher.update(&(bytes.len() as u64).to_le_bytes()); hasher.update(bytes); }
    hasher.update(&[u8::from(old.1), u8::from(new.1)]);
    let token = hasher.finalize().to_hex().to_string();
    let (prefix, hunks) = if binary || truncated { (String::new(), Vec::new()) } else { hunks(std::str::from_utf8(&patch)?)? };
    let rendered: Vec<_> = hunks.iter().enumerate().map(|(index, hunk)| {
        let mut old = hunk.old_start; let mut new = hunk.new_start;
        let lines: Vec<_> = hunk.lines.iter().enumerate().map(|(line_index, line)| {
            let (kind, old_line, new_line) = match line.as_bytes().first() {
                Some(b'+') => { let number = new; new += 1; ("addition", None, Some(number)) },
                Some(b'-') => { let number = old; old += 1; ("deletion", Some(number), None) },
                Some(b' ') => { let a = old; let b = new; old += 1; new += 1; ("context", Some(a), Some(b)) },
                _ => ("context", None, None),
            };
            json!({"id": format!("h{index}:l{line_index}"), "kind": kind, "text": line.get(1..).unwrap_or_default(), "old_line": old_line, "new_line": new_line})
        }).collect();
        json!({"id": format!("h{index}"), "header": hunk.header, "lines": lines})
    }).collect();
    Ok(Diff { value: json!({"path": path, "view": view, "commit": commit, "token": token, "old_text": if binary || truncated { String::new() } else { text(&old.0) }, "new_text": if binary || truncated { String::new() } else { text(&new.0) }, "old_present": old.1, "new_present": new.1, "binary": binary, "truncated": truncated, "hunks": rendered}), prefix, hunks })
}

pub fn diff(repo: Repo, path: &str, view: &str, commit: Option<&str>) -> Result<Value> {
    let mut git = Git::new(repo); let result = diff_inner(&mut git, path, view, commit)?;
    Ok(git.finish(result.value))
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum StageRequest {
    Files { paths: Vec<String> },
    All,
    Hunks { path: String, token: String, hunk_ids: Vec<String> },
    Lines { path: String, token: String, line_ids: Vec<String> },
}

fn selected_patch(diff: &Diff, hunk_ids: Option<&[String]>, line_ids: Option<&[String]>, reverse: bool) -> Result<String> {
    let mut valid = std::collections::HashSet::new();
    let mut patch = diff.prefix.clone();
    let mut delta: isize = 0;
    for (index, hunk) in diff.hunks.iter().enumerate() {
        let all = hunk_ids.is_some_and(|ids| ids.contains(&format!("h{index}")));
        if all { valid.insert(format!("h{index}")); }
        let mut body = String::new(); let mut old_count = 0; let mut new_count = 0; let mut changed = false;
        let mut preceding_kept = false;
        for (number, line) in hunk.lines.iter().enumerate() {
            let id = format!("h{index}:l{number}");
            let selected = all || line_ids.is_some_and(|ids| ids.contains(&id));
            match line.as_bytes().first() {
                Some(b'+') => {
                    preceding_kept = selected || reverse;
                    if selected { valid.insert(id); body.push_str(line); new_count += 1; changed = true; }
                    else if reverse { body.push(' '); body.push_str(&line[1..]); old_count += 1; new_count += 1; }
                }
                Some(b'-') => {
                    preceding_kept = selected || !reverse;
                    if selected { valid.insert(id); body.push_str(line); old_count += 1; changed = true; }
                    else if !reverse { body.push(' '); body.push_str(&line[1..]); old_count += 1; new_count += 1; }
                }
                Some(b' ') => { preceding_kept = true; body.push_str(line); old_count += 1; new_count += 1; }
                Some(b'\\') if preceding_kept => body.push_str(line),
                _ => {}
            }
        }
        if !changed { continue; }
        let old_start = if reverse {
            if new_count == 0 { (hunk.new_start as isize + delta + 1).max(1) as usize }
            else if old_count == 0 { (hunk.new_start as isize + delta - 1).max(0) as usize }
            else { (hunk.new_start as isize + delta).max(1) as usize }
        } else { hunk.old_start };
        let new_start = if reverse { hunk.new_start }
            else if old_count == 0 { (old_start as isize + delta + 1).max(1) as usize }
            else if new_count == 0 { (old_start as isize + delta - 1).max(0) as usize }
            else { (old_start as isize + delta).max(1) as usize };
        patch.push_str(&format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@\n{body}"));
        delta += if reverse { old_count as isize - new_count as isize } else { new_count as isize - old_count as isize };
    }
    let requested = hunk_ids.or(line_ids).context("missing selection")?;
    ensure!(!requested.is_empty() && requested.iter().all(|id| valid.contains(id)), "invalid or non-change selection ID");
    // A partial selection of a new/deleted file leaves that index entry alive.
    // Remove the whole-file creation/deletion metadata in that case.
    let partial_new = diff.value["old_present"] == false && patch.lines().any(|line| line.starts_with(' '));
    let partial_deleted = diff.value["new_present"] == false && patch.lines().any(|line| line.starts_with(' '));
    if partial_new || partial_deleted {
        let old_header = diff.prefix.lines().find(|line| line.starts_with("--- ")).unwrap_or_default();
        let new_header = diff.prefix.lines().find(|line| line.starts_with("+++ ")).unwrap_or_default();
        patch = patch.split_inclusive('\n').filter(|line| !line.starts_with("new file mode ") && !line.starts_with("deleted file mode "))
            .map(|line| {
                if partial_new && line.starts_with("--- /dev/null") { format!("--- {}\n", &new_header[4..]) }
                else if partial_deleted && line.starts_with("+++ /dev/null") { format!("+++ {}\n", &old_header[4..]) }
                else { line.to_owned() }
            }).collect();
    }
    Ok(patch)
}

pub fn stage(repo: Repo, request: StageRequest, reverse: bool) -> Result<Value> {
    let mut git = Git::new(repo);
    match request {
        StageRequest::Files { paths } => {
            ensure!(!paths.is_empty(), "select at least one file");
            let paths: Vec<_> = paths.iter().map(|path| git.path(path)).collect::<Result<_>>()?;
            let head = git.head()?;
            let mut args = if reverse && head.is_some() { vec!["--literal-pathspecs", "restore", "--staged", "--source=HEAD", "--"] }
                else if reverse { vec!["--literal-pathspecs", "rm", "--cached", "--ignore-unmatch", "--"] }
                else { vec!["--literal-pathspecs", "add", "-A", "--"] };
            args.extend(paths.iter().map(String::as_str)); git.checked(&args, None)?;
        }
        StageRequest::All => {
            if reverse {
                if git.head()?.is_some() { git.checked(&["restore", "--staged", "--source=HEAD", ":/"], None)?; }
                else { git.checked(&["rm", "--cached", "-r", "--ignore-unmatch", "--", "."], None)?; }
            } else { git.checked(&["add", "-A"], None)?; }
        }
        StageRequest::Hunks { path, token, hunk_ids } => {
            apply_selection(&mut git, &path, &token, Some(&hunk_ids), None, reverse)?;
        }
        StageRequest::Lines { path, token, line_ids } => {
            apply_selection(&mut git, &path, &token, None, Some(&line_ids), reverse)?;
        }
    }
    Ok(git.finish(json!({"changed": true})))
}

fn apply_selection(git: &mut Git, path: &str, token: &str, hunks: Option<&[String]>, lines: Option<&[String]>, reverse: bool) -> Result<()> {
    let diff = diff_inner(git, path, if reverse { "staged" } else { "working" }, None)?;
    if diff.value["token"].as_str() != Some(token) { return Err(ActionError { code: "stale_diff", message: "File or index changed; refresh the diff".into(), commands: git.commands.clone() }.into()); }
    let patch = selected_patch(&diff, hunks, lines, reverse)?;
    let mut args = vec!["apply", "--cached", "--recount", "--whitespace=nowarn"];
    if reverse { args.push("--reverse"); }
    git.checked(&args, Some(patch.as_bytes()))?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitRequest { pub message: String, #[serde(default)] pub amend: bool }

pub fn commit(repo: Repo, request: CommitRequest, executable: &Path) -> Result<Value> {
    use std::io::Write;
    ensure!(!request.message.trim().is_empty() && !request.message.contains('\0'), "commit message is empty or invalid");
    let mut message = tempfile::NamedTempFile::new()?; message.write_all(request.message.as_bytes())?;
    message.as_file().sync_all()?;
    let mut args = vec!["--verify".into(), "--file".into(), message.path().as_os_str().to_owned()];
    if request.amend { args.push("--amend".into()); }
    let outcome = crate::commit::execute_commit(repo.clone(), executable, args, crate::commit::StagePolicy::ExistingIndex, false, true)?;
    if outcome.code != 0 { return Err(ActionError { code: "git_failed", message: "Git commit failed; see the command log".into(), commands: outcome.commands }.into()); }
    let mut git = Git::new(repo); git.commands = outcome.commands;
    let head = git.head()?;
    Ok(git.finish(json!({"head": head})))
}
