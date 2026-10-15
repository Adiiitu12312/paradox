#![cfg(unix)]
use paradox_core::{repo::Repo, store::LoopStore};
use std::{fs, process::Command};

#[test]
fn alternate_root_spelling_preserves_symlink_components_below_the_root() {
    let temp=tempfile::tempdir().unwrap();let root=temp.path().join("real");let alias=temp.path().join("alias");fs::create_dir(&root).unwrap();
    assert!(Command::new("git").args(["init","-q"]).current_dir(&root).status().unwrap().success());
    fs::write(root.join("a.ts"),"//> first\nfirst\n").unwrap();assert!(Command::new("git").args(["add","a.ts"]).current_dir(&root).status().unwrap().success());
    std::os::unix::fs::symlink(&root,&alias).unwrap();std::os::unix::fs::symlink(&root,root.join("self")).unwrap();
    let repo=Repo::discover(&["-C".into(),root.as_os_str().to_owned()]).unwrap();
    assert_eq!(repo.relative(&alias.join("a.ts")).unwrap(),"a.ts");assert_eq!(repo.relative(&alias.join("self/a.ts")).unwrap(),"self/a.ts");
    let mut store=LoopStore::open(repo).unwrap();assert_eq!(store.capture(&[alias.join("a.ts")]).unwrap().saved,1);
    fs::write(root.join("a.ts"),"//> second\nsecond\n").unwrap();assert_eq!(store.capture(&[alias.join("self/a.ts")]).unwrap().saved,0);assert_eq!(store.loops("a.ts").unwrap().len(),1);
}
