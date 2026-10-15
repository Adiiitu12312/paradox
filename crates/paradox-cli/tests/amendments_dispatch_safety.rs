//! Native commands and owned routing are exercised only in OS-temp repos.
use paradox_core::{repo::Repo,store::LoopStore};
use std::{fs,path::Path,process::{Command,Output}};

fn command(root:&Path,exe:&str)->Command{
    let mut command=Command::new(exe);command.current_dir(root);
    for(name,_)in std::env::vars_os(){if name.to_string_lossy().starts_with("GIT_"){command.env_remove(name);}}
    command.env("GIT_CONFIG_NOSYSTEM","1").env("GIT_CONFIG_GLOBAL",root.join("absent-global"))
        .env("GIT_AUTHOR_NAME","Fixture").env("GIT_AUTHOR_EMAIL","fixture@example.invalid")
        .env("GIT_COMMITTER_NAME","Fixture").env("GIT_COMMITTER_EMAIL","fixture@example.invalid");command
}
fn ok(output:Output)->String{assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));String::from_utf8(output.stdout).unwrap()}
fn git(root:&Path,args:&[&str])->String{ok(command(root,"git").args(args).output().unwrap())}
fn px(root:&Path,args:&[&str])->String{ok(command(root,env!("CARGO_BIN_EXE_paradox")).args(args).output().unwrap())}
fn repo(root:&Path)->Repo{Repo::discover(&["-C".into(),root.as_os_str().to_owned()]).unwrap()}
fn fixture()->tempfile::TempDir{
    let temp=tempfile::tempdir().unwrap();let root=temp.path();git(root,&["init","-q","--initial-branch=main"]);
    for(k,v)in[("core.autocrlf","false"),("commit.gpgsign","false"),("core.hooksPath",".git/no-hooks")]{git(root,&["config",k,v]);}
    for name in ["file.txt","loop","commit","-odd"]{fs::write(root.join(name),"base\n").unwrap();}git(root,&["add","-A"]);git(root,&["commit","-qm","base"]);temp
}
fn enable(root:&Path){
    px(root,&["loop","--stats"]);let store=repo(root).store;
    fs::write(store.join("config.toml"),"safety_loop_commands=['reset']\n").unwrap();
    // An owned command activates a manually edited configuration; ordinary
    // pass-through deliberately cannot discover TOML without the tiny flag.
    px(root,&["loop","--stats"]);assert!(store.join("safety_enabled").is_file());
}

#[test]
fn opt_in_reset_captures_only_modified_tracked_work_before_forwarding(){
    let temp=fixture();let root=temp.path();enable(root);
    fs::write(root.join("file.txt"),"preserved pre-reset bytes\n").unwrap();fs::write(root.join("untracked.txt"),"not tracked\n").unwrap();
    px(root,&["reset","--hard","HEAD"]);assert_eq!(fs::read_to_string(root.join("file.txt")).unwrap(),"base\n");
    let store=LoopStore::open(repo(root)).unwrap();let loops=store.loops("file.txt").unwrap();assert_eq!(loops.len(),2);assert_eq!(loops[0].kind,"safety");assert_eq!(loops[0].message,"before git reset (working)");
    let version=store.resolve("file.txt",&loops[0].id.to_string()).unwrap();assert_eq!(store.version_bytes("file.txt",&version).unwrap(),b"preserved pre-reset bytes\n");assert!(store.loops("untracked.txt").unwrap().is_empty());
}

#[test]
fn absent_flag_never_opens_malformed_config_or_spawns_a_discovery_git(){
    let temp=fixture();let root=temp.path();let store=repo(root).store;fs::create_dir_all(&store).unwrap();fs::write(store.join("config.toml"),"this is not TOML [").unwrap();
    let trace=root.join("trace.jsonl");let output=command(root,env!("CARGO_BIN_EXE_paradox")).args(["status","--porcelain=v2","-z"]).env("GIT_TRACE2_EVENT",&trace).output().unwrap();assert!(output.status.success());
    let events:Vec<serde_json::Value>=fs::read_to_string(trace).unwrap().lines().map(|line|serde_json::from_str(line).unwrap()).collect();assert_eq!(events.iter().filter(|event|event["event"]=="start").count(),1,"only the actual forwarded native Git may start");assert!(!store.join("log").exists());assert!(!store.join("index.sqlite3").exists());assert_eq!(fs::read_to_string(store.join("config.toml")).unwrap(),"this is not TOML [");
}

#[test]
fn force_git_always_bypasses_enabled_safety_and_original_arguments_remain_native(){
    let temp=fixture();let root=temp.path();enable(root);fs::write(root.join("file.txt"),"intentionally unprotected by escape\n").unwrap();
    px(root,&["git","reset","--hard","HEAD"]);assert_eq!(fs::read_to_string(root.join("file.txt")).unwrap(),"base\n");assert!(LoopStore::open(repo(root)).unwrap().loops("file.txt").unwrap().is_empty());
    let native=command(root,"git").args(["-c","alias.exact=!printf 'literal bytes\\n'","exact"]).output().unwrap();
    let wrapped=command(root,env!("CARGO_BIN_EXE_paradox")).args(["-c","alias.exact=!printf 'literal bytes\\n'","exact"]).output().unwrap();assert_eq!(native.status.code(),wrapped.status.code());assert_eq!(native.stdout,wrapped.stdout);assert_eq!(native.stderr,wrapped.stderr);
}

#[test]
fn file_escape_commits_owned_and_option_looking_filenames_with_git_globals(){
    let temp=fixture();let root=temp.path();let outside=tempfile::tempdir().unwrap();
    for name in ["loop","commit","-odd"]{
        fs::write(root.join(name),format!("changed {name}\n")).unwrap();fs::write(root.join("file.txt"),"unrelated working edit\n").unwrap();
        px(outside.path(),&["-C",root.to_str().unwrap(),"-c","commit.gpgsign=false","--",name,"commit","-m",&format!("only {name}")]);
        assert_eq!(git(root,&["show",&format!("HEAD:{name}")]),format!("changed {name}\n"));assert_eq!(git(root,&["show","HEAD:file.txt"]),"base\n");assert_eq!(fs::read_to_string(root.join("file.txt")).unwrap(),"unrelated working edit\n");
    }
}

#[test]
fn linked_worktree_and_explicit_git_directory_find_the_shared_flag_without_git_discovery(){
    let temp=fixture();let root=temp.path();enable(root);let linked_parent=tempfile::tempdir().unwrap();let linked=linked_parent.path().join("linked");git(root,&["worktree","add","-qb","linked",linked.to_str().unwrap()]);
    fs::write(linked.join("file.txt"),"linked safety\n").unwrap();px(&linked,&["reset","--hard","HEAD"]);
    let linked_store=LoopStore::open(repo(&linked)).unwrap();assert_eq!(linked_store.loops("file.txt").unwrap().len(),2);drop(linked_store);assert!(LoopStore::open(repo(root)).unwrap().loops("file.txt").unwrap().is_empty());
    fs::write(root.join("file.txt"),"explicit gitdir safety\n").unwrap();let git_dir=root.join(".git");
    px(linked_parent.path(),&["--git-dir",git_dir.to_str().unwrap(),"--work-tree",root.to_str().unwrap(),"reset","--hard","HEAD"]);
    assert_eq!(LoopStore::open(repo(root)).unwrap().loops("file.txt").unwrap().len(),2);
}

#[test]
fn opt_in_reset_preserves_a_staged_only_version_when_working_already_matches_head(){
    let temp=fixture();let root=temp.path();enable(root);
    fs::write(root.join("file.txt"),"unique staged-only bytes\n").unwrap();git(root,&["add","file.txt"]);fs::write(root.join("file.txt"),"base\n").unwrap();
    px(root,&["reset","--hard","HEAD"]);
    let store=LoopStore::open(repo(root)).unwrap();let loops=store.loops("file.txt").unwrap();let staged=loops.iter().find(|row|row.message=="before git reset (staged)").unwrap();let version=store.resolve("file.txt",&staged.id.to_string()).unwrap();assert_eq!(store.version_bytes("file.txt",&version).unwrap(),b"unique staged-only bytes\n");assert!(loops.iter().all(|row|!row.pinned));assert_eq!(loops[0].message,"before git reset (working)");assert_eq!(fs::read_to_string(root.join("file.txt")).unwrap(),"base\n");
}
