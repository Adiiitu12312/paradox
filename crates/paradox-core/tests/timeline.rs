use paradox_core::repo::Repo;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

struct Fixture(TempDir);

impl Fixture {
    fn new() -> Self {
        let fixture = Self(tempfile::tempdir().unwrap());
        fixture.git(&["init", "-q", "--initial-branch=main"]);
        fixture.git(&["config", "core.hooksPath", ".git/no-hooks"]);
        fixture.git(&["config", "core.autocrlf", "false"]);
        fixture.git(&["config", "commit.gpgsign", "false"]);
        fixture
    }

    fn command(&self, arguments: &[&str]) -> Output {
        let mut command = Command::new("git");
        command.args(arguments).current_dir(self.0.path());
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.0.path().join("absent-global"))
            .env("GIT_AUTHOR_NAME", "Paradox Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Paradox Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .unwrap()
    }

    fn git(&self, arguments: &[&str]) {
        let output = self.command(arguments);
        assert!(
            output.status.success(),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write(&self, name: &str, content: &str) {
        let path = self.0.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn commit(&self, names: &[&str]) {
        let mut arguments = vec!["add", "--"];
        arguments.extend_from_slice(names);
        self.git(&arguments);
        self.git(&["commit", "-qm", "fixture"]);
    }

    fn repo_at(&self, directory: &Path) -> Repo {
        Repo::discover(&[OsString::from("-C"), directory.as_os_str().to_owned()]).unwrap()
    }

    fn repo(&self) -> Repo {
        self.repo_at(self.0.path())
    }

    fn explicit(&self, names: &[&str]) {
        let repo = self.repo();
        let worktrees = BTreeMap::from([(repo.worktree, names)]);
        fs::create_dir_all(&repo.store).unwrap();
        fs::write(
            repo.store.join("tracked.toml"),
            toml::to_string(&BTreeMap::from([("worktrees", worktrees)])).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn zero_through_five_commits_use_current_head_and_the_exact_net_boundary() {
    let fixture = Fixture::new();
    assert!(fixture.repo().timeline_paths().unwrap().is_empty());
    assert!(!fixture.0.path().join(".git/paradox").exists());
    fixture.write("root.ts", "root\n");
    fixture.git(&["add", "--", "root.ts"]);
    let reused_repo = fixture.repo();
    assert_eq!(reused_repo.timeline_paths().unwrap(), ["root.ts"]);
    fixture.git(&["commit", "-qm", "root"]);
    assert!(reused_repo.timeline_paths().unwrap().is_empty());
    for count in 2_i32..=5 {
        let name = format!("commit-{count}.ts");
        fixture.write(&name, "new\n");
        fixture.commit(&[&name]);
        let expected: Vec<_> = (2.max(count - 2)..=count)
            .map(|number| format!("commit-{number}.ts"))
            .collect();
        assert_eq!(
            reused_repo.timeline_paths().unwrap(),
            expected,
            "{count} commits"
        );
    }
}

#[test]
fn net_reverts_are_excluded_but_dirty_tracked_paths_and_recent_deletions_survive() {
    let fixture = Fixture::new();
    for name in ["net.ts", "deleted.ts", "old.ts", "working.ts"] {
        fixture.write(name, "base\n");
    }
    fixture.commit(&["net.ts", "deleted.ts", "old.ts", "working.ts"]);
    fixture.write("net.ts", "intermediate\n");
    fixture.write("working.ts", "committed change\n");
    fixture.git(&["rm", "--", "deleted.ts"]);
    fixture.git(&["mv", "--", "old.ts", "new.ts"]);
    fixture.commit(&["net.ts", "working.ts"]);
    fixture.write("net.ts", "base\n");
    fixture.commit(&["net.ts"]);
    fixture.git(&["commit", "--allow-empty", "-qm", "fourth"]);
    // This working file equals the diff boundary, but differs from HEAD.
    fixture.write("working.ts", "base\n");
    fixture.write("staged.ts", "staged\n");
    fixture.git(&["add", "--", "staged.ts"]);
    for number in 0..20 {
        fixture.write(&format!("untracked/{number}/noise.ts"), "noise\n");
    }
    assert_eq!(
        fixture.repo().timeline_paths().unwrap(),
        ["deleted.ts", "new.ts", "old.ts", "staged.ts", "working.ts",]
    );
}

#[test]
fn explicit_new_paths_are_included_but_clean_committed_entries_are_not_dirty() {
    let fixture = Fixture::new();
    fixture.write("quiet.ts", "base\n");
    fixture.commit(&["quiet.ts"]);
    fixture.write("extra.ts", "extra\n");
    fixture.write("eventually-git.ts", "eventual\n");
    fixture.write("raw-untracked.ts", "noise\n");
    fixture.explicit(&["quiet.ts", "extra.ts", "eventually-git.ts"]);
    let repo = fixture.repo();
    assert_eq!(
        repo.timeline_paths().unwrap(),
        ["eventually-git.ts", "extra.ts"]
    );
    // Keep the preexisting public API's capture behavior intact.
    assert!(repo.changes().unwrap().dirty.contains("quiet.ts"));
    assert!(repo.tracked(&[]).unwrap().contains("extra.ts"));
    fixture.git(&["add", "--", "eventually-git.ts"]);
    assert_eq!(
        repo.timeline_paths().unwrap(),
        ["eventually-git.ts", "extra.ts"]
    );
    fixture.git(&["commit", "-qm", "now tracked"]);
    for _ in 0..3 {
        fixture.git(&["commit", "--allow-empty", "-qm", "advance boundary"]);
    }
    assert_eq!(repo.timeline_paths().unwrap(), ["extra.ts"]);
    fixture.write("eventually-git.ts", "dirty\n");
    assert_eq!(
        repo.timeline_paths().unwrap(),
        ["eventually-git.ts", "extra.ts"]
    );
}

#[test]
fn nul_paths_and_rename_records_remain_root_relative_from_a_subdirectory() {
    let fixture = Fixture::new();
    let original = "nested/space 日本語.ts";
    let renamed = "nested/renamed 日本語.ts";
    let names = vec![original, "--leading-dash.ts"];
    #[cfg(unix)]
    let names = {
        let mut names = names;
        names.extend([
            "nested/line\nname.ts",
            "nested/tab\tname.ts",
            "nested/literal\\name.ts",
        ]);
        names
    };
    for name in &names {
        fixture.write(name, "base\n");
    }
    fixture.commit(&names);
    fixture.git(&["mv", "--", original, renamed]);
    for name in names.iter().skip(1) {
        fixture.write(name, "dirty\n");
    }
    let mut expected: Vec<_> = names.into_iter().map(str::to_owned).collect();
    expected.push(renamed.to_owned());
    expected.sort_unstable();
    let nested = fixture.repo_at(&fixture.0.path().join("nested"));
    assert_eq!(nested.timeline_paths().unwrap(), expected);
    assert_eq!(
        nested.timeline_paths().unwrap(),
        fixture.repo().timeline_paths().unwrap()
    );
}

#[test]
fn unmerged_porcelain_records_are_candidates() {
    let fixture = Fixture::new();
    fixture.write("nested/conflict file.ts", "base\n");
    fixture.commit(&["nested/conflict file.ts"]);
    fixture.git(&["checkout", "-qb", "side"]);
    fixture.write("nested/conflict file.ts", "side\n");
    fixture.commit(&["nested/conflict file.ts"]);
    fixture.git(&["checkout", "-q", "main"]);
    fixture.write("nested/conflict file.ts", "main\n");
    fixture.commit(&["nested/conflict file.ts"]);
    assert!(
        !fixture
            .command(&["merge", "--no-edit", "side"])
            .status
            .success()
    );
    assert_eq!(
        fixture.repo().timeline_paths().unwrap(),
        ["nested/conflict file.ts"]
    );
}
