use paradox_core::{repo::Repo,ui_actions::{self,Action,Cancellation,ResetMode,Operation,ResolveRequest},ui_git::ActionError,store::LoopStore};
use serde_json::{Value,json};
use std::{fs,ffi::OsString,process::{Command,Output}};
struct Fixture(tempfile::TempDir);
impl Fixture {
 fn new()->Self{let f=Self(tempfile::tempdir().unwrap());f.git(&["init","-q","--initial-branch=main"]);for(k,v)in[("user.name","Fixture"),("user.email","fixture@example.invalid"),("core.autocrlf","false"),("core.hooksPath",".git/no-hooks"),("commit.gpgsign","false")]{f.git(&["config",k,v]);}f}
 fn output(&self,args:&[&str])->Output{let mut command=Command::new("git");command.current_dir(self.0.path()).args(args);for(name,_)in std::env::vars_os(){if name.to_string_lossy().starts_with("GIT_"){command.env_remove(name);}}command.env("GIT_CONFIG_NOSYSTEM","1").env("GIT_CONFIG_GLOBAL",self.0.path().join("absent")).output().unwrap()}
 fn git(&self,args:&[&str])->String{let output=self.output(args);assert!(output.status.success(),"{args:?}: {}",String::from_utf8_lossy(&output.stderr));String::from_utf8(output.stdout).unwrap()}
 fn repo(&self)->Repo{Repo::discover(&[OsString::from("-C"),self.0.path().as_os_str().to_owned()]).unwrap()}
 fn write(&self,path:&str,bytes:&[u8]){fs::write(self.0.path().join(path),bytes).unwrap();}
 fn commit(&self,message:&str){self.git(&["add","-A"]);self.git(&["commit","-qm",message]);}
 fn initial(&self){self.write("a.ts",b"base\n");self.commit("initial");}
 fn run(&self,action:Action)->Value{let plan=ui_actions::preview(self.repo(),action.clone()).unwrap();ui_actions::execute(self.repo(),action,Some(&plan),Cancellation::new(),|_|{}).unwrap()}
}

#[test]
fn branch_tag_state_and_noninteractive_history_actions_are_native_git(){
 let f=Fixture::new();f.initial();
 f.run(Action::BranchCreate{name:"topic".into(),start:None});f.run(Action::BranchSwitch{name:"topic".into()});f.write("topic.ts",b"topic\n");f.commit("topic commit");let topic=f.git(&["rev-parse","HEAD"]).trim().to_owned();
 f.run(Action::TagCreate{name:"v1".into(),target:None,message:Some("annotated".into())});
 let state=ui_actions::state(f.repo()).unwrap();assert!(state["branches"].as_array().unwrap().iter().any(|row|row["name"]=="topic"&&row["current"]==true));assert_eq!(state["tags"][0]["sha"],topic);
 f.run(Action::BranchSwitch{name:"main".into()});f.run(Action::Merge{revision:"topic".into()});assert_eq!(f.git(&["rev-parse","HEAD"]).trim(),topic);
 f.run(Action::BranchRename{name:"topic".into(),new_name:"renamed".into()});f.run(Action::BranchDelete{name:"renamed".into(),force:false});f.run(Action::TagDelete{name:"v1".into()});
 f.run(Action::Revert{revision:topic});assert!(!f.0.path().join("topic.ts").exists());
 let error=ui_actions::preview(f.repo(),Action::Rebase{onto:"main".into(),interactive:true}).unwrap_err();assert!(error.to_string().contains("paradox rebase -i"));
}

#[test]
fn stash_create_apply_pop_and_drop_use_stable_object_ids(){
 let f=Fixture::new();f.initial();f.write("a.ts",b"changed\n");f.write("new.ts",b"new\n");
 f.run(Action::StashCreate{message:Some("literal -- stash".into()),include_untracked:true});assert_eq!(fs::read(f.0.path().join("a.ts")).unwrap(),b"base\n");assert!(!f.0.path().join("new.ts").exists());
 let state=ui_actions::state(f.repo()).unwrap();let oid=state["stashes"][0]["oid"].as_str().unwrap().to_owned();f.run(Action::StashApply{oid:oid.clone(),pop:false});assert_eq!(fs::read(f.0.path().join("a.ts")).unwrap(),b"changed\n");
 f.git(&["reset","--hard","HEAD"]);fs::remove_file(f.0.path().join("new.ts")).unwrap();f.run(Action::StashApply{oid,pop:true});assert!(ui_actions::state(f.repo()).unwrap()["stashes"].as_array().unwrap().is_empty());
 f.run(Action::StashCreate{message:None,include_untracked:true});let state=ui_actions::state(f.repo()).unwrap();f.run(Action::StashDrop{oid:state["stashes"][0]["oid"].as_str().unwrap().into()});assert!(ui_actions::state(f.repo()).unwrap()["stashes"].as_array().unwrap().is_empty());
}

#[test]
fn destructive_plan_stales_and_safety_preserves_distinct_staged_and_working_bytes(){
 let f=Fixture::new();f.initial();f.write("a.ts",b"staged\n");f.git(&["add","a.ts"]);f.write("a.ts",b"working\n");
 let action=Action::Reset{target:"HEAD".into(),mode:ResetMode::Hard};let plan=ui_actions::preview(f.repo(),action.clone()).unwrap();assert_eq!(plan.safety.len(),2);
 f.write("a.ts",b"new working\n");let error=ui_actions::execute(f.repo(),action.clone(),Some(&plan),Cancellation::new(),|_|{}).unwrap_err();assert_eq!(error.downcast_ref::<ActionError>().unwrap().code,"stale_plan");assert_eq!(fs::read(f.0.path().join("a.ts")).unwrap(),b"new working\n");
 let result=f.run(action);assert_eq!(fs::read(f.0.path().join("a.ts")).unwrap(),b"base\n");
 let store=LoopStore::open(f.repo()).unwrap();for expected in [b"staged\n".as_slice(),b"new working\n".as_slice()]{assert!(result["safety_loops"].as_array().unwrap().iter().any(|row|{let id=row["id"].as_i64().unwrap();let version=store.resolve("a.ts",&id.to_string()).unwrap();store.version_bytes("a.ts",&version).unwrap()==expected}));}
}

#[test]
fn skipped_protection_is_explicit_and_discard_has_a_recoverable_safety_loop(){
 let f=Fixture::new();f.initial();f.write("a.ts",b"discard me\n");let action=Action::Discard{paths:vec!["a.ts".into()]};let result=f.run(action);assert_eq!(fs::read(f.0.path().join("a.ts")).unwrap(),b"base\n");assert!(!result["safety_loops"].as_array().unwrap().is_empty());
 f.write(".env",b"SECRET=do-not-store\n");f.git(&["add",".env"]);f.write("binary.bin",b"\0binary");f.git(&["add","binary.bin"]);
 let plan=ui_actions::preview(f.repo(),Action::Reset{target:"HEAD".into(),mode:ResetMode::Hard}).unwrap();assert!(plan.skipped.iter().any(|entry|entry.path==".env"&&entry.reason.as_ref().unwrap().contains("secret")));assert!(plan.skipped.iter().any(|entry|entry.path=="binary.bin"&&entry.reason.as_ref().unwrap().contains("binary")));
}

#[test]
fn conflict_manual_resolution_is_stale_checked_protected_and_marked_resolved(){
 let f=Fixture::new();f.initial();f.git(&["checkout","-qb","topic"]);f.write("a.ts",b"theirs\n");f.commit("theirs");f.git(&["checkout","-q","main"]);f.write("a.ts",b"ours\n");f.commit("ours");assert!(!f.output(&["merge","--no-edit","topic"]).status.success());
 let conflict=ui_actions::conflict(f.repo(),"a.ts").unwrap();assert_eq!(conflict["ours"]["text"],"ours\n");assert_eq!(conflict["theirs"]["text"],"theirs\n");
 let request=|token:String|serde_json::from_value::<ResolveRequest>(json!({"path":"a.ts","token":token,"choice":"manual","result":"resolved\n","mark_resolved":true})).unwrap();
 let old=conflict["token"].as_str().unwrap().to_owned();f.write("a.ts",b"external edit\n");assert!(ui_actions::resolve_conflict(f.repo(),request(old)).is_err());
 let conflict=ui_actions::conflict(f.repo(),"a.ts").unwrap();let result=ui_actions::resolve_conflict(f.repo(),request(conflict["token"].as_str().unwrap().into())).unwrap();assert_eq!(result["resolved"],true);assert!(f.git(&["ls-files","--unmerged"]).is_empty());assert_eq!(f.git(&["show",":a.ts"]),"resolved\n");
 f.run(Action::Continue{operation:Operation::Merge});assert!(f.git(&["rev-list","--parents","-1","HEAD"]).split_whitespace().count()==3);
}

#[test]
fn blame_and_history_follow_native_commits_and_renames(){
 let f=Fixture::new();f.initial();f.git(&["mv","a.ts","renamed é.ts"]);f.commit("rename");let history=ui_actions::history(f.repo(),"renamed é.ts",None).unwrap();assert_eq!(history["commits"].as_array().unwrap().len(),2);let blame=ui_actions::blame(f.repo(),"renamed é.ts",None).unwrap();assert_eq!(blame["lines"][0]["text"],"base\n");assert_eq!(blame["lines"][0]["summary"],"initial");assert!(ui_actions::blame(f.repo(),"../outside",None).is_err());
}

#[test]
fn local_bare_remote_streams_progress_and_rejects_an_outdated_explicit_lease(){
 let f=Fixture::new();f.initial();let bare=tempfile::tempdir().unwrap();let status=Command::new("git").args(["init","--bare","-q"]).arg(bare.path()).status().unwrap();assert!(status.success());f.git(&["remote","add","origin",bare.path().to_str().unwrap()]);
 let mut progress=Vec::new();ui_actions::execute(f.repo(),Action::Push{remote:"origin".into(),branch:"main".into(),force_with_lease:false},None,Cancellation::new(),|event|progress.push(serde_json::to_value(event).unwrap())).unwrap();assert!(progress.iter().any(|event|event["type"]=="command"));assert!(progress.iter().any(|event|event["type"]=="exit"));
 f.run(Action::Fetch{remote:"origin".into(),prune:true});let force=Action::Push{remote:"origin".into(),branch:"main".into(),force_with_lease:true};let plan=ui_actions::preview(f.repo(),force.clone()).unwrap();
 // Advance only the remote; the local preview remains valid but its exact
 // observed-tip lease must reject the race rather than overwrite new history.
 f.write("a.ts",b"second\n");f.commit("second");let second=f.git(&["rev-parse","HEAD"]);f.git(&["push","origin","main"]);f.git(&["reset","--hard","HEAD~1"]);
 let fresh=ui_actions::preview(f.repo(),force.clone()).unwrap();
 let expected=plan.commands.iter().flat_map(|row|row.stdout.lines()).find(|line|line.len()==40).unwrap_or_default();assert!(!expected.is_empty());
 // Restore the old tracking observation without altering the remote.
 f.git(&["update-ref","refs/remotes/origin/main",expected]);let stale_remote=ui_actions::preview(f.repo(),force.clone()).unwrap();
 let error=ui_actions::execute(f.repo(),force,Some(&stale_remote),Cancellation::new(),|_|{}).unwrap_err();assert_eq!(error.downcast_ref::<ActionError>().unwrap().code,"git_failed");
 let remote=Command::new("git").arg("--git-dir").arg(bare.path()).args(["rev-parse","refs/heads/main"]).output().unwrap();assert_eq!(String::from_utf8(remote.stdout).unwrap(),second);drop(fresh);
}
