use crate::commit::CommandRecord;
use anyhow::{Result,Context};
use serde::Serialize;
use std::{sync::{Arc,Mutex,Condvar},process::{Command,Stdio},io::Read};

#[derive(Clone,Default)]
pub struct Cancellation(Arc<(Mutex<bool>,Condvar)>);
impl Cancellation {
 pub fn new()->Self{Self::default()}
 pub fn cancel(&self){let (lock,notify)=&*self.0;*lock.lock().unwrap_or_else(|p|p.into_inner())=true;notify.notify_all();}
 pub fn is_cancelled(&self)->bool{*self.0.0.lock().unwrap_or_else(|p|p.into_inner())}
}
#[derive(Debug,Serialize)]
#[serde(tag="type",rename_all="snake_case")]
pub enum ProgressEvent {Command{argv:Vec<String>},Stdout{text:String},Stderr{text:String},Exit{exit_code:i32}}

#[cfg(unix)]
struct Tree(u32);
#[cfg(unix)]
impl Tree {
 fn prepare(command:&mut Command){use std::os::unix::process::CommandExt;command.process_group(0);}
 fn attach(child:&std::process::Child)->Result<Self>{Ok(Self(child.id()))}
 fn terminate(&self){unsafe extern "C"{fn kill(pid:i32,signal:i32)->i32;}unsafe{kill(-(self.0 as i32),9);}}
}

#[cfg(windows)]
struct Tree(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
unsafe impl Send for Tree{}
#[cfg(windows)]
unsafe impl Sync for Tree{}
#[cfg(windows)]
impl Tree {
 fn prepare(command:&mut Command){use std::os::windows::process::CommandExt;command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);}
 fn attach(child:&std::process::Child)->Result<Self>{
  use std::os::windows::io::AsRawHandle;
  use windows_sys::Win32::{Foundation::{CloseHandle,INVALID_HANDLE_VALUE},System::{JobObjects::*,Diagnostics::ToolHelp::*,Threading::*}};
  // The initial thread remains suspended until its kill-on-close job owns the
  // process. No hook/SSH child can escape ownership during job assignment.
  unsafe {
   let job=CreateJobObjectW(std::ptr::null(),std::ptr::null());anyhow::ensure!(!job.is_null(),"create operation job: {}",std::io::Error::last_os_error());
   let tree=Self(job);let mut limits:JOBOBJECT_EXTENDED_LIMIT_INFORMATION=std::mem::zeroed();limits.BasicLimitInformation.LimitFlags=JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
   anyhow::ensure!(SetInformationJobObject(job,JobObjectExtendedLimitInformation,&limits as *const _ as *const _,std::mem::size_of_val(&limits) as u32)!=0,"configure operation job: {}",std::io::Error::last_os_error());
   anyhow::ensure!(AssignProcessToJobObject(job,child.as_raw_handle())!=0,"assign operation job: {}",std::io::Error::last_os_error());
   let snapshot=CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD,0);anyhow::ensure!(snapshot!=INVALID_HANDLE_VALUE,"operation thread snapshot failed");
   let mut entry:THREADENTRY32=std::mem::zeroed();entry.dwSize=std::mem::size_of_val(&entry) as u32;let mut found=false;
   let mut next=Thread32First(snapshot,&mut entry);
   while next!=0 {
    if entry.th32OwnerProcessID==child.id(){let thread=OpenThread(THREAD_SUSPEND_RESUME,0,entry.th32ThreadID);if !thread.is_null(){let resumed=ResumeThread(thread);CloseHandle(thread);found=resumed!=u32::MAX;break;}}
    next=Thread32Next(snapshot,&mut entry);
   }
   CloseHandle(snapshot);anyhow::ensure!(found,"resume owned Git operation failed");Ok(tree)
  }
 }
 fn terminate(&self){unsafe{windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0,1);}}
}
#[cfg(windows)]
impl Drop for Tree{fn drop(&mut self){unsafe{windows_sys::Win32::Foundation::CloseHandle(self.0);}}}

enum Event{Output(bool,Vec<u8>),Done(std::io::Result<std::process::ExitStatus>),ReadError(std::io::Error)}
struct Finish {done:Arc<(Mutex<bool>,Condvar)>,cancel:Cancellation,tree:Arc<Tree>}
impl Drop for Finish {fn drop(&mut self){self.tree.terminate();let _requested=self.cancel.0.0.lock().unwrap_or_else(|p|p.into_inner());*self.done.0.lock().unwrap_or_else(|p|p.into_inner())=true;self.cancel.0.1.notify_all();}}
pub(super) fn run(mut command:Command,cancel:Cancellation,on_event:&mut(impl FnMut(ProgressEvent)+Send))->Result<CommandRecord>{
 anyhow::ensure!(!cancel.is_cancelled(),"operation cancelled");
 let argv:Vec<_>=std::iter::once(command.get_program()).chain(command.get_args()).map(|v|v.to_string_lossy().into_owned()).collect();
 command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());Tree::prepare(&mut command);
 let mut child=command.spawn()?;let tree=match Tree::attach(&child){Ok(tree)=>Arc::new(tree),Err(error)=>{let _=child.kill();let _=child.wait();return Err(error);}};
 on_event(ProgressEvent::Command{argv:argv.clone()});
 let stdout=child.stdout.take().context("missing Git stdout")?;let stderr=child.stderr.take().context("missing Git stderr")?;
 let finished=Arc::new((Mutex::new(false),Condvar::new()));
 let result=std::thread::scope(|scope|->Result<CommandRecord>{
  let _finish=Finish{done:finished.clone(),cancel:cancel.clone(),tree:tree.clone()};
  let (sender,receiver)=std::sync::mpsc::sync_channel(32);
  for (is_error,mut reader) in [(false,Box::new(stdout) as Box<dyn Read+Send>),(true,Box::new(stderr) as Box<dyn Read+Send>)]{
   let sender=sender.clone();scope.spawn(move||{let mut bytes=[0;4096];loop{match reader.read(&mut bytes){Ok(0)=>break,Ok(n)=>{if sender.send(Event::Output(is_error,bytes[..n].to_vec())).is_err(){break;}},Err(error)=>{let _=sender.send(Event::ReadError(error));break;}}}});
  }
  let waiter_sender=sender.clone();let waiter_tree=tree.clone();scope.spawn(move||{let status=child.wait();waiter_tree.terminate();let _=waiter_sender.send(Event::Done(status));});
  let cancel_tree=tree.clone();let done=finished.clone();let cancel_signal=cancel.clone();
  scope.spawn(move||{
   let (lock,notify)=&*cancel_signal.0;let mut requested=lock.lock().unwrap_or_else(|p|p.into_inner());
   while !*requested&&!(*done.0.lock().unwrap_or_else(|p|p.into_inner())){requested=notify.wait(requested).unwrap_or_else(|p|p.into_inner());}
   if *requested{cancel_tree.terminate();}
  });
  drop(sender);let mut status=None;let mut out=Vec::new();let mut err=Vec::new();let mut failure=None;
  for event in receiver {
   match event {
    Event::Output(is_error,bytes)=>{let destination=if is_error{&mut err}else{&mut out};let remaining=65_536usize.saturating_sub(destination.len());destination.extend_from_slice(&bytes[..remaining.min(bytes.len())]);let text=String::from_utf8_lossy(&bytes).into_owned();on_event(if is_error{ProgressEvent::Stderr{text}}else{ProgressEvent::Stdout{text}});},
    Event::Done(result)=>{let _requested=cancel.0.0.lock().unwrap_or_else(|p|p.into_inner());*finished.0.lock().unwrap_or_else(|p|p.into_inner())=true;cancel.0.1.notify_all();status=Some(result?);},
    Event::ReadError(error)=>{tree.terminate();failure=Some(error);}
   }
  }
  if let Some(error)=failure{return Err(error.into());}let code=status.context("Git operation exited without status")?.code().unwrap_or(1);on_event(ProgressEvent::Exit{exit_code:code});
  Ok(CommandRecord{argv,exit_code:code,stdout:String::from_utf8_lossy(&out).into_owned(),stderr:String::from_utf8_lossy(&err).into_owned()})
 });
 // Wake the cancellation waiter even if a stream/callback operation failed.
 *finished.0.lock().unwrap_or_else(|p|p.into_inner())=true;cancel.0.1.notify_all();result
}

#[cfg(test)]
mod tests {
 use super::*;
 fn python()->Command{Command::new(if cfg!(windows){"python"}else{"python3"})}
 #[test]
 fn progress_streams_both_pipes_and_preserves_actual_exit(){
  let mut command=python();command.args(["-c","import sys; print('out',flush=True); print('err',file=sys.stderr,flush=True); sys.exit(7)"]);
  let mut events=Vec::new();let result=run(command,Cancellation::new(),&mut|event|events.push(event)).unwrap();
  assert_eq!(result.exit_code,7);assert!(result.stdout.contains("out"));assert!(result.stderr.contains("err"));assert!(matches!(events.first(),Some(ProgressEvent::Command{..})));assert!(matches!(events.last(),Some(ProgressEvent::Exit{exit_code:7})));
 }
 #[test]
 fn cancellation_owns_and_reaps_descendants(){
  let directory=tempfile::tempdir().unwrap();let script=directory.path().join("owned_child.py");
  std::fs::write(&script,"import subprocess,sys,time\nchild=subprocess.Popen([sys.executable,'-c','import time; time.sleep(120)'])\nprint('READY '+str(child.pid),flush=True)\ntime.sleep(120)\n").unwrap();
  let mut command=python();command.arg(script);let cancel=Cancellation::new();let trigger=cancel.clone();let mut text=String::new();let mut child=None;
  #[cfg(windows)]
  let mut process=0usize;
  let started=std::time::Instant::now();
  let result=run(command,cancel,&mut|event|{if let ProgressEvent::Stdout{text:chunk}=event{text.push_str(&chunk);if child.is_none()&&text.contains('\n'){let pid=text.trim().strip_prefix("READY ").unwrap().parse::<u32>().unwrap();child=Some(pid);
   #[cfg(windows)]
   {use windows_sys::Win32::System::Threading::*;process=unsafe{OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION|0x0010_0000,0,pid)} as usize;assert_ne!(process,0);}
   trigger.cancel();}}}).unwrap();
  assert!(child.is_some());assert_ne!(result.exit_code,0);assert!(started.elapsed()<std::time::Duration::from_secs(10));
  #[cfg(unix)]
  {let pid=child.unwrap();let deadline=std::time::Instant::now()+std::time::Duration::from_secs(5);loop{
   // An adopted zombie has exited too; it cannot execute or retain resources
   // and its new OS parent owns reaping it.
   let output=Command::new("ps").args(["-o","stat=","-p",&pid.to_string()]).output().unwrap();let state=String::from_utf8_lossy(&output.stdout);
   if state.trim().is_empty()||state.trim_start().starts_with('Z'){break;}assert!(std::time::Instant::now()<deadline,"descendant still running: {pid} {state}");std::thread::sleep(std::time::Duration::from_millis(20));
  }}
  #[cfg(windows)]
  unsafe{use windows_sys::Win32::{Foundation::CloseHandle,System::Threading::*};let process=process as windows_sys::Win32::Foundation::HANDLE;assert_eq!(WaitForSingleObject(process,5000),0);let mut code=0;assert_ne!(GetExitCodeProcess(process,&mut code),0);CloseHandle(process);assert_ne!(code,259);}
 }
}
