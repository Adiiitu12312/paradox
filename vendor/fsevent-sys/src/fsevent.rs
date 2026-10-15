#![allow(non_upper_case_globals, non_camel_case_types)]

use crate::core_foundation::{
    Boolean, CFAbsoluteTime, CFAllocatorCopyDescriptionCallBack, CFAllocatorRef,
    CFAllocatorReleaseCallBack, CFAllocatorRetainCallBack, CFArrayRef, CFIndex, CFRunLoopRef,
    CFStringRef, CFTimeInterval,
};
use libc::dev_t;
use std::os::raw::{c_uint, c_void};

pub type FSEventStreamRef = *mut c_void;
pub type ConstFSEventStreamRef = *const c_void;

pub type FSEventStreamCallback = extern "C" fn(
    FSEventStreamRef,               // ConstFSEventStreamRef streamRef
    *mut c_void,                    // void *clientCallBackInfo
    usize,                          // size_t numEvents
    *mut c_void,                    // void *eventPaths
    *const FSEventStreamEventFlags, // const FSEventStreamEventFlags eventFlags[]
    *const FSEventStreamEventId,    // const FSEventStreamEventId eventIds[]
);

pub type FSEventStreamEventId = u64;

pub type FSEventStreamCreateFlags = c_uint;

pub type FSEventStreamEventFlags = c_uint;

pub const kFSEventStreamEventIdSinceNow: FSEventStreamEventId = 0xFFFFFFFFFFFFFFFF;

pub const kFSEventStreamCreateFlagNone: FSEventStreamCreateFlags = 0x00000000;
pub const kFSEventStreamCreateFlagUseCFTypes: FSEventStreamCreateFlags = 0x00000001;
pub const kFSEventStreamCreateFlagNoDefer: FSEventStreamCreateFlags = 0x00000002;
pub const kFSEventStreamCreateFlagWatchRoot: FSEventStreamCreateFlags = 0x00000004;
pub const kFSEventStreamCreateFlagIgnoreSelf: FSEventStreamCreateFlags = 0x00000008;
pub const kFSEventStreamCreateFlagFileEvents: FSEventStreamCreateFlags = 0x00000010;
pub const kFSEventStreamCreateFlagMarkSelf: FSEventStreamCreateFlags = 0x00000020;
pub const kFSEventStreamCreateFlagUseExtendedData: FSEventStreamCreateFlags = 0x00000040;
pub const kFSEventStreamCreateFlagFullHistory: FSEventStreamCreateFlags = 0x00000080;

pub const kFSEventStreamEventFlagNone: FSEventStreamEventFlags = 0x00000000;
pub const kFSEventStreamEventFlagMustScanSubDirs: FSEventStreamEventFlags = 0x00000001;
pub const kFSEventStreamEventFlagUserDropped: FSEventStreamEventFlags = 0x00000002;
pub const kFSEventStreamEventFlagKernelDropped: FSEventStreamEventFlags = 0x00000004;
pub const kFSEventStreamEventFlagEventIdsWrapped: FSEventStreamEventFlags = 0x00000008;
pub const kFSEventStreamEventFlagHistoryDone: FSEventStreamEventFlags = 0x00000010;
pub const kFSEventStreamEventFlagRootChanged: FSEventStreamEventFlags = 0x00000020;
pub const kFSEventStreamEventFlagMount: FSEventStreamEventFlags = 0x00000040;
pub const kFSEventStreamEventFlagUnmount: FSEventStreamEventFlags = 0x00000080;
pub const kFSEventStreamEventFlagItemCreated: FSEventStreamEventFlags = 0x00000100;
pub const kFSEventStreamEventFlagItemRemoved: FSEventStreamEventFlags = 0x00000200;
pub const kFSEventStreamEventFlagItemInodeMetaMod: FSEventStreamEventFlags = 0x00000400;
pub const kFSEventStreamEventFlagItemRenamed: FSEventStreamEventFlags = 0x00000800;
pub const kFSEventStreamEventFlagItemModified: FSEventStreamEventFlags = 0x00001000;
pub const kFSEventStreamEventFlagItemFinderInfoMod: FSEventStreamEventFlags = 0x00002000;
pub const kFSEventStreamEventFlagItemChangeOwner: FSEventStreamEventFlags = 0x00004000;
pub const kFSEventStreamEventFlagItemXattrMod: FSEventStreamEventFlags = 0x00008000;
pub const kFSEventStreamEventFlagItemIsFile: FSEventStreamEventFlags = 0x00010000;
pub const kFSEventStreamEventFlagItemIsDir: FSEventStreamEventFlags = 0x00020000;
pub const kFSEventStreamEventFlagItemIsSymlink: FSEventStreamEventFlags = 0x00040000;
pub const kFSEventStreamEventFlagOwnEvent: FSEventStreamEventFlags = 0x00080000;
pub const kFSEventStreamEventFlagItemIsHardlink: FSEventStreamEventFlags = 0x00100000;
pub const kFSEventStreamEventFlagItemIsLastHardlink: FSEventStreamEventFlags = 0x00200000;
pub const kFSEventStreamEventFlagItemCloned: FSEventStreamEventFlags = 0x00400000;

#[repr(C)]
pub struct FSEventStreamContext {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: Option<CFAllocatorRetainCallBack>,
    pub release: Option<CFAllocatorReleaseCallBack>,
    pub copy_description: Option<CFAllocatorCopyDescriptionCallBack>,
}

// Paradox diagnostic prototype: upstream 4.1.0 declarations retain their ABI,
// but CoreServices opens only when ensure_loaded() or an API is first used.
// The upstream MIT license is preserved alongside this source.
use std::ffi::CStr;
use std::sync::OnceLock;
const FRAMEWORK: &[u8] = b"/System/Library/Frameworks/CoreServices.framework/CoreServices\0";

fn loader_error(context: &str) -> String {
    let error = unsafe { libc::dlerror() };
    if error.is_null() {
        context.to_owned()
    } else {
        format!(
            "{}: {}",
            context,
            unsafe { CStr::from_ptr(error) }.to_string_lossy()
        )
    }
}
unsafe fn symbol(handle: *mut c_void, name: &'static [u8]) -> Result<*mut c_void, String> {
    unsafe {
        libc::dlerror();
    }
    let address = unsafe { libc::dlsym(handle, name.as_ptr().cast()) };
    if address.is_null() {
        Err(loader_error(&format!(
            "CoreServices symbol {} unavailable",
            String::from_utf8_lossy(&name[..name.len() - 1])
        )))
    } else {
        Ok(address)
    }
}
trait Unavailable {
    fn unavailable() -> Self;
}
fn unavailable<T: Unavailable>() -> T {
    T::unavailable()
}
impl Unavailable for () {
    fn unavailable() {}
}
impl Unavailable for u64 {
    fn unavailable() -> Self {
        0
    }
}
impl Unavailable for u8 {
    fn unavailable() -> Self {
        0
    }
}
impl Unavailable for i32 {
    fn unavailable() -> Self {
        0
    }
}
impl<T> Unavailable for *mut T {
    fn unavailable() -> Self {
        std::ptr::null_mut()
    }
}
impl<T> Unavailable for *const T {
    fn unavailable() -> Self {
        std::ptr::null()
    }
}

macro_rules! lazy_api {
    ($(fn $name:ident($($argument:ident: $kind:ty),* $(,)?) $(-> $result:ty)?;)+) => {
        #[allow(non_snake_case)]
        struct Api {
            // Intentionally retain the dlopen reference until process exit.
            // This lifetime-only handle is never dereferenced or closed.
            _handle: usize,
            $($name: unsafe extern "C" fn($($kind),*) $(-> $result)?,)+
        }
        impl Api {
            unsafe fn load() -> Result<Self, String> {
                let handle = unsafe { libc::dlopen(FRAMEWORK.as_ptr().cast(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
                if handle.is_null() { return Err(loader_error("Cannot load CoreServices framework")); }
                // Retain the single reference on partial failure too. No
                // partial function table is ever published to callers.
                Ok(Self { _handle: handle as usize,
                    $($name: unsafe { std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($kind),*) $(-> $result)?>(
                        symbol(handle, concat!(stringify!($name), "\0").as_bytes())?
                    ) },)+
                })
            }
        }
        static API: OnceLock<Result<Api, String>> = OnceLock::new();
        /// Resolve all CoreServices symbols once, before creating notify's
        /// FSEvents watcher. Missing framework/symbols return an ordinary error.
        pub fn ensure_loaded() -> Result<(), String> {
            match API.get_or_init(|| unsafe { Api::load() }) {
                Ok(_) => Ok(()), Err(error) => Err(error.clone()),
            }
        }
        $(
            /// # Safety
            /// The pointer, ownership and lifetime requirements are identical
            /// to Apple's function with the same name. Call ensure_loaded()
            /// before creating a watcher to handle loader errors gracefully.
            #[allow(non_snake_case)]
            pub unsafe extern "C" fn $name($($argument: $kind),*) $(-> $result)? {
                match API.get_or_init(|| unsafe { Api::load() }) {
                    Ok(api) => unsafe { (api.$name)($($argument),*) },
                    // Never panic or call a missing function address. The
                    // required preflight reports errors before construction.
                    Err(_) => unavailable(),
                }
            }
        )+
    }
}
lazy_api! {
    fn FSEventStreamCopyDescription(stream_ref: ConstFSEventStreamRef) -> CFStringRef;
    fn FSEventStreamCopyPathsBeingWatched(streamRef: ConstFSEventStreamRef) -> CFArrayRef;
    fn FSEventStreamCreate(allocator: CFAllocatorRef, callback: FSEventStreamCallback, context: *const FSEventStreamContext, pathsToWatch: CFArrayRef, sinceWhen: FSEventStreamEventId, latency: CFTimeInterval, flags: FSEventStreamCreateFlags) -> FSEventStreamRef;
    fn FSEventStreamCreateRelativeToDevice(allocator: CFAllocatorRef, callback: FSEventStreamCallback, context: *const FSEventStreamContext, deviceToWatch: dev_t, pathsToWatchRelativeToDevice: CFArrayRef, sinceWhen: FSEventStreamEventId, latency: CFTimeInterval, flags: FSEventStreamCreateFlags) -> FSEventStreamRef;
    fn FSEventStreamFlushAsync(stream_ref: FSEventStreamRef) -> FSEventStreamEventId;
    fn FSEventStreamFlushSync(streamRef: FSEventStreamRef);
    fn FSEventStreamGetDeviceBeingWatched(stream_ref: ConstFSEventStreamRef) -> dev_t;
    fn FSEventStreamGetLatestEventId(stream_ref: ConstFSEventStreamRef) -> FSEventStreamEventId;
    fn FSEventStreamInvalidate(stream_ref: FSEventStreamRef);
    fn FSEventStreamRelease(stream_ref: FSEventStreamRef);
    fn FSEventStreamRetain(stream_ref: FSEventStreamRef);
    fn FSEventStreamScheduleWithRunLoop(stream_ref: FSEventStreamRef, run_loop: CFRunLoopRef, run_loop_mode: CFStringRef);
    fn FSEventStreamSetExclusionPaths(stream_ref: FSEventStreamRef, paths_to_exclude: CFArrayRef) -> Boolean;
    fn FSEventStreamShow(stream_ref: FSEventStreamRef);
    fn FSEventStreamStart(stream_ref: FSEventStreamRef) -> Boolean;
    fn FSEventStreamStop(stream_ref: FSEventStreamRef);
    fn FSEventStreamUnscheduleFromRunLoop(stream_ref: FSEventStreamRef, run_loop: CFRunLoopRef, run_loop_mode: CFStringRef);
    fn FSEventsGetCurrentEventId() -> FSEventStreamEventId;
    fn FSEventsGetLastEventIdForDeviceBeforeTime(dev: dev_t, time: CFAbsoluteTime) -> FSEventStreamEventId;
    fn FSEventsPurgeEventsForDeviceUpToEventId(dev: dev_t, eventId: FSEventStreamEventId) -> Boolean;

}
#[cfg(test)]
mod lazy_tests {
    use super::*;
    #[test]
    fn resolves_complete_api_and_reports_a_missing_symbol() {
        ensure_loaded().expect("macOS CoreServices preflight");
        let api = API.get().unwrap().as_ref().unwrap();
        let missing = unsafe {
            symbol(
                api._handle as *mut c_void,
                b"ParadoxDiagnosticMissingFSEventsSymbol\0",
            )
        };
        assert!(missing.is_err());
        assert!(ensure_loaded().is_ok());
    }
}
