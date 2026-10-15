use paradox_core::{repo::Repo, ui_git::{self, StageRequest}};
use serde_json::Value;
use std::{ffi::OsString, fs, process::Command};
use tempfile::TempDir;

struct Fixture(TempDir);
impl Fixture {
    fn new() -> Self {
        let fixture = Self(tempfile::tempdir().unwrap());
        fixture.git(&["init", "-q", "--initial-branch=main"]);
        for (key,value) in [("user.name","Fixture"),("user.email","fixture@example.invalid"),("core.autocrlf","false"),("commit.gpgsign","false"),("core.hooksPath",".git/no-hooks")] { fixture.git(&["config",key,value]); }
        fixture
    }
    fn git(&self,args:&[&str])->String {
        let mut command=Command::new("git"); command.current_dir(self.0.path()).args(args);
        for (name,_) in std::env::vars_os() { if name.to_string_lossy().starts_with("GIT_") {command.env_remove(name);} }
        let output=command.env("GIT_CONFIG_NOSYSTEM","1").env("GIT_CONFIG_GLOBAL",self.0.path().join("absent")).output().unwrap();
        assert!(output.status.success(),"{args:?}: {}",String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    }
    fn repo(&self)->Repo {Repo::discover(&[OsString::from("-C"),self.0.path().as_os_str().to_owned()]).unwrap()}
    fn write(&self,path:&str,bytes:&str) {fs::write(self.0.path().join(path),bytes).unwrap();}
    fn initial(&self,bytes:&str) { self.write("a.ts",bytes);self.git(&["add","a.ts"]);self.git(&["commit","-qm","initial"]); }
    fn diff(&self,view:&str)->Value {ui_git::diff(self.repo(),"a.ts",view,None).unwrap()}
    fn lines(&self,diff:&Value,wanted:&[&str],reverse:bool) {
        let ids=diff["hunks"].as_array().unwrap().iter().flat_map(|h|h["lines"].as_array().unwrap()).filter(|line| wanted.contains(&line["text"].as_str().unwrap().trim_end_matches('\n'))).map(|line|line["id"].as_str().unwrap().to_owned()).collect();
        ui_git::stage(self.repo(),StageRequest::Lines{path:"a.ts".into(),token:diff["token"].as_str().unwrap().into(),line_ids:ids},reverse).unwrap();
    }
}

#[test]
fn line_stage_and_unstage_preserve_working_bytes_and_unselected_changes() {
    let f=Fixture::new();f.initial("one\ntwo\nthree\nfour\n");f.write("a.ts","ONE\ntwo\nthree\nFOUR\n");
    f.lines(&f.diff("working"),&["one","ONE"],false);
    assert_eq!(f.git(&["show",":a.ts"]),"ONE\ntwo\nthree\nfour\n");
    assert_eq!(fs::read_to_string(f.0.path().join("a.ts")).unwrap(),"ONE\ntwo\nthree\nFOUR\n");
    f.lines(&f.diff("working"),&["four","FOUR"],false);
    f.lines(&f.diff("staged"),&["one","ONE"],true);
    assert_eq!(f.git(&["show",":a.ts"]),"one\ntwo\nthree\nFOUR\n");
}

#[test]
fn partial_new_file_and_partial_deleted_file_support_both_directions() {
    let f=Fixture::new();f.write("a.ts","one\ntwo\nthree\n");
    f.lines(&f.diff("working"),&["one","three"],false);
    assert_eq!(f.git(&["show",":a.ts"]),"one\nthree\n");
    f.lines(&f.diff("staged"),&["one"],true);
    assert_eq!(f.git(&["show",":a.ts"]),"three\n");
    f.git(&["add","a.ts"]);f.git(&["commit","-qm","initial"]);
    fs::remove_file(f.0.path().join("a.ts")).unwrap();
    f.lines(&f.diff("working"),&["two"],false);
    assert_eq!(f.git(&["show",":a.ts"]),"one\nthree\n");
    f.git(&["add","-A"]);
    f.lines(&f.diff("staged"),&["two"],true);
    assert_eq!(f.git(&["show",":a.ts"]),"two\n");
}

#[test]
fn stale_tokens_and_client_patch_injection_are_rejected() {
    let f=Fixture::new();f.initial("one\n");f.write("a.ts","two\n");let diff=f.diff("working");f.write("a.ts","three\n");
    let error=ui_git::stage(f.repo(),StageRequest::Hunks{path:"a.ts".into(),token:diff["token"].as_str().unwrap().into(),hunk_ids:vec!["h0".into()]},false).unwrap_err();
    assert_eq!(error.downcast_ref::<ui_git::ActionError>().unwrap().code,"stale_diff");
    assert_eq!(f.git(&["show",":a.ts"]),"one\n");
    assert!(serde_json::from_value::<StageRequest>(serde_json::json!({"mode":"hunks","path":"a.ts","token":"x","hunk_ids":["h0"],"patch":"malicious"})).is_err());
    assert!(ui_git::diff(f.repo(),"../external","working",None).is_err());
}

#[test]
fn status_graph_and_commit_details_use_native_git_paths_and_metadata() {
    let f=Fixture::new();assert!(ui_git::graph(f.repo(),None).unwrap()["commits"].as_array().unwrap().is_empty());
    f.initial("one\n");f.git(&["mv","a.ts","renamed é.ts"]);f.git(&["commit","-qm","rename <script>"]);
    let graph=ui_git::graph(f.repo(),None).unwrap();assert_eq!(graph["commits"][0]["subject"],"rename <script>");
    let detail=ui_git::commit_detail(f.repo(),graph["commits"][0]["sha"].as_str().unwrap()).unwrap();
    assert_eq!(detail["files"][0]["path"],"renamed é.ts");assert_eq!(detail["files"][0]["old_path"],"a.ts");
    let rename_diff=ui_git::diff(f.repo(),"renamed é.ts","working",graph["commits"][0]["sha"].as_str()).unwrap();
    assert_eq!(rename_diff["old_text"],"one\n");assert_eq!(rename_diff["new_text"],"one\n");
    f.write("untracked.ts","new");let status=ui_git::status(f.repo()).unwrap();assert!(status["files"].as_array().unwrap().iter().any(|entry|entry["path"]=="untracked.ts" && entry["untracked"]==true));
}

#[cfg(unix)]
#[test]
fn symlink_diff_shows_target_and_refuses_external_parent() {
    let f=Fixture::new();let outside=tempfile::tempdir().unwrap();fs::write(outside.path().join("secret"),"outside bytes").unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret"),f.0.path().join("a.ts")).unwrap();
    let diff=f.diff("working");assert!(!diff["new_text"].as_str().unwrap().contains("outside bytes"));
    std::os::unix::fs::symlink(outside.path(),f.0.path().join("external")).unwrap();assert!(ui_git::diff(f.repo(),"external/secret","working",None).is_err());
}

#[test]
fn first_graph_page_on_one_hundred_thousand_commits_is_bounded_and_fast() {
    use std::io::Write;
    use std::process::Stdio;
    let f=Fixture::new();
    let mut child=Command::new("git").args(["fast-import","--quiet"]).current_dir(f.0.path())
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    let mut input=child.stdin.take().unwrap();
    let writer=std::thread::spawn(move || {
        for index in 0..100_000 {
            let message=format!("commit {index}");
            write!(input,"commit refs/heads/main\ncommitter Fixture <fixture@example.invalid> {} +0000\ndata {}\n{}\n\n",1_700_000_000+index,message.len(),message).unwrap();
        }
        writeln!(input,"done").unwrap();
    });
    writer.join().unwrap();let output=child.wait_with_output().unwrap();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    let repo=f.repo();let started=std::time::Instant::now();let page=ui_git::graph(repo,None).unwrap();let elapsed=started.elapsed();
    assert_eq!(page["commits"].as_array().unwrap().len(),200);assert_eq!(page["next_cursor"],"200");
    assert!(elapsed<std::time::Duration::from_millis(300),"first 100k-commit graph page took {elapsed:?}");
    let next=ui_git::graph(f.repo(),page["next_cursor"].as_str()).unwrap();assert_ne!(page["commits"][0]["sha"],next["commits"][0]["sha"]);
}
