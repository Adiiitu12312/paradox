//! Flag-gated opt-in capture before native Git. The disabled path performs
//! filesystem hint checks only: no config, SQLite, logging, or Git subprocess.
use paradox_cli::dispatch::{self,Route};
use std::{ffi::OsString,path::{Path,PathBuf},io::Read};

fn small_text(path:&Path)->Option<String>{
    let metadata=std::fs::metadata(path).ok()?;if !metadata.is_file()||metadata.len()>1_048_576{return None;}
    let mut text=String::new();std::fs::File::open(path).ok()?.take(1_048_577).read_to_string(&mut text).ok()?;
    (text.len()<=1_048_576).then_some(text)
}
fn absolute(base:&Path,path:impl AsRef<Path>)->PathBuf{let path=path.as_ref();if path.is_absolute(){path.to_owned()}else{base.join(path)}}
fn git_directory(path:PathBuf)->Option<PathBuf>{
    if path.is_dir(){return Some(path);}
    let content=small_text(&path)?;let target=content.strip_prefix("gitdir:")?.trim();
    (!target.is_empty()).then(||absolute(path.parent().unwrap_or_else(||Path::new(".")),target))
}
fn hint_from(args:&[OsString],index:usize,mut cwd:PathBuf,mut git_dir:Option<OsString>,common:Option<OsString>)->Option<PathBuf>{
    let mut bare=false;let mut at=0;
    while at<index{
        let text=args[at].to_str()?;
        if text=="-C"{let value=args.get(at+1)?;if !value.is_empty(){cwd=absolute(&cwd,value);}at+=2;continue;}
        if let Some(value)=text.strip_prefix("-C").filter(|_|text.len()>2){cwd=absolute(&cwd,value);at+=1;continue;}
        if text=="--git-dir"{git_dir=Some(args.get(at+1)?.clone());at+=2;continue;}
        if let Some(value)=text.strip_prefix("--git-dir="){git_dir=Some(value.into());at+=1;continue;}
        if text=="--bare"{bare=true;}
        if matches!(text,"-c"|"--work-tree"|"--namespace"|"--config-env"){at+=2;}else{at+=1;}
    }
    // Git's GIT_COMMON_DIR value is interpreted from the effective cwd; a
    // linked-worktree commondir file is relative to its individual gitdir.
    if let Some(common)=common{return Some(absolute(&cwd,common).join("paradox"));}
    let directory=if let Some(directory)=git_dir{git_directory(absolute(&cwd,directory))?}else if bare{cwd.clone()}else{
        let mut current=cwd.clone();loop{
            let marker=current.join(".git");
            if marker.exists(){break git_directory(marker)?;}
            if current.join("HEAD").is_file()&&current.join("objects").is_dir()&&current.join("refs").is_dir(){break current;}
            if !current.pop(){return None;}
        }
    };
    let common=if let Some(common)=small_text(&directory.join("commondir")){absolute(&directory,common.trim_end_matches(['\r','\n']))}else{directory};
    Some(common.join("paradox"))
}

/// Fast non-authoritative filesystem discovery; real Git validates the repo
/// only after its returned store has an existing safety_enabled flag.
pub fn store_hint(args:&[OsString])->Option<PathBuf>{
    let index=dispatch::git_subcommand_index(args)?;
    hint_from(args,index,std::env::current_dir().ok()?,std::env::var_os("GIT_DIR"),std::env::var_os("GIT_COMMON_DIR"))
}

pub fn before_git(args:&[OsString]){
    if dispatch::route(args)!=Route::Git{return;}
    let Some(index)=dispatch::git_subcommand_index(args)else{return;};
    let Some(command)=args[index].to_str()else{return;};
    let Some(hint)=store_hint(args)else{return;};
    if !hint.join("safety_enabled").is_file(){return;}
    let result=(||->anyhow::Result<()>{
        let repo=paradox_core::repo::Repo::discover(dispatch::global_args(args,index))?;
        // The hint does not authorize reading a different repository's TOML.
        if !repo.store.join("safety_enabled").is_file(){return Ok(());}
        let config=paradox_core::config::Config::load(&repo.store)?;
        if !config.safety_loop_commands.iter().any(|entry|entry==command){return Ok(());}
        paradox_core::git_safety::capture_before(repo,command)?;
        Ok(())
    })();
    if let Err(error)=result{paradox_core::store::log_at(&hint,&format!("opt-in safety before {command}: {error:#}"));}
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn filesystem_hints_follow_c_gitfiles_commondir_and_explicit_paths(){
        let root=tempfile::tempdir().unwrap();let repo=root.path().join("repo");let git=repo.join(".git");std::fs::create_dir_all(&git).unwrap();
        let args=vec!["-C".into(),repo.as_os_str().to_owned(),"status".into()];assert_eq!(hint_from(&args,2,root.path().to_owned(),None,None),Some(git.join("paradox")));
        let linked=root.path().join("linked");let worktree_git=git.join("worktrees/linked");std::fs::create_dir_all(&linked).unwrap();std::fs::create_dir_all(&worktree_git).unwrap();
        std::fs::write(linked.join(".git"),format!("gitdir: {}\n",worktree_git.display())).unwrap();std::fs::write(worktree_git.join("commondir"),"../..\n").unwrap();
        let args=vec!["reset".into()];let hint=hint_from(&args,0,linked,None,None).unwrap();assert_eq!(std::fs::canonicalize(hint.parent().unwrap()).unwrap(),std::fs::canonicalize(&git).unwrap());
        let args=vec!["--git-dir".into(),git.as_os_str().to_owned(),"status".into()];assert_eq!(hint_from(&args,2,root.path().to_owned(),None,None),Some(git.join("paradox")));
        assert_eq!(hint_from(&args,2,root.path().to_owned(),None,Some("common".into())),Some(root.path().join("common/paradox")));
    }
}
