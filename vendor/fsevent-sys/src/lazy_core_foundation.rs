// Diagnostic-only late-bound CoreFoundation API. Upstream ABI/types remain in
// core_foundation.rs; references to exported DATA use their real native storage.
use super::*;
use std::ffi::{c_void, CStr};
use std::sync::OnceLock;
const FRAMEWORK: &[u8] = b"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation\0";
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
            "CoreFoundation symbol {} unavailable",
            String::from_utf8_lossy(&name[..name.len() - 1])
        )))
    } else {
        Ok(address)
    }
}
trait Unavailable {
    fn unavailable() -> Self;
}
impl Unavailable for () {
    fn unavailable() {}
}
impl Unavailable for bool {
    fn unavailable() -> Self {
        false
    }
}
impl Unavailable for i64 {
    fn unavailable() -> Self {
        0
    }
}
impl Unavailable for u8 {
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
            _handle: usize,
            type_array_callbacks: usize,
            run_loop_default_mode: usize,
            $($name: unsafe extern "C" fn($($kind),*) $(-> $result)?,)+
        }
        impl Api {
            unsafe fn load() -> Result<Self, String> {
                let handle = unsafe { libc::dlopen(FRAMEWORK.as_ptr().cast(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
                if handle.is_null() { return Err(loader_error("Cannot load CoreFoundation framework")); }
                // Retain handle even on partial failure; no partial table escapes.
                Ok(Self { _handle: handle as usize,
                    type_array_callbacks: unsafe { symbol(handle,b"kCFTypeArrayCallBacks\0")? } as usize,
                    run_loop_default_mode: unsafe { symbol(handle,b"kCFRunLoopDefaultMode\0")? } as usize,
                    $($name: unsafe { std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($kind),*) $(-> $result)?>(symbol(handle, concat!(stringify!($name),"\0").as_bytes())?) },)+
                })
            }
        }
        static API: OnceLock<Result<Api,String>> = OnceLock::new();
        pub fn ensure_loaded() -> Result<(),String> {
            API.get_or_init(|| unsafe { Api::load() }).as_ref().map(|_| ()).map_err(Clone::clone)
        }
        $(
            /// # Safety
            /// Identical pointer, ownership and lifetime requirements to the native API.
            #[allow(non_snake_case)]
            pub unsafe extern "C" fn $name($($argument: $kind),*) $(-> $result)? {
                match API.get_or_init(|| unsafe { Api::load() }) {
                    Ok(api) => unsafe { (api.$name)($($argument),*) },
                    Err(_) => Unavailable::unavailable(),
                }
            }
        )+
    }
}
lazy_api! {
    fn CFRelease(res: CFRef);
    fn CFShow(res: CFRef);
    fn CFCopyDescription(cf: CFRef) -> CFStringRef;

    fn CFRunLoopRun();
    fn CFRunLoopStop(run_loop: CFRunLoopRef);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopIsWaiting(run_loop: CFRunLoopRef) -> Boolean;

    fn CFArrayCreateMutable(
        allocator: CFRef,
        capacity: CFIndex,
        callbacks: *const CFArrayCallBacks,
    ) -> CFMutableArrayRef;
    fn CFArrayInsertValueAtIndex(arr: CFMutableArrayRef, position: CFIndex, element: CFRef);
    fn CFArrayAppendValue(aff: CFMutableArrayRef, element: CFRef);
    fn CFArrayGetCount(arr: CFMutableArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(arr: CFMutableArrayRef, index: CFIndex) -> CFRef;

    fn CFURLCreateFileReferenceURL(
        allocator: CFRef,
        url: CFURLRef,
        err: *mut CFErrorRef,
    ) -> CFURLRef;
    fn CFURLCreateFilePathURL(
        allocator: CFRef,
        url: CFURLRef,
        err: *mut CFErrorRef,
    ) -> CFURLRef;
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: CFRef,
        path: *const ::std::os::raw::c_char,
        len: CFIndex,
        is_directory: bool,
    ) -> CFURLRef;
    fn CFURLCopyAbsoluteURL(res: CFURLRef) -> CFURLRef;
    fn CFURLCopyLastPathComponent(res: CFURLRef) -> CFStringRef;
    fn CFURLCreateCopyDeletingLastPathComponent(allocator: CFRef, url: CFURLRef) -> CFURLRef;
    fn CFURLCreateCopyAppendingPathComponent(
        allocation: CFRef,
        url: CFURLRef,
        path: CFStringRef,
        is_directory: bool,
    ) -> CFURLRef;
    fn CFURLCopyFileSystemPath(anUrl: CFURLRef, path_style: CFURLPathStyle) -> CFStringRef;

    fn CFURLResourceIsReachable(res: CFURLRef, err: *mut CFErrorRef) -> bool;

    fn CFShowStr(str: CFStringRef);
    fn CFStringGetCString(
        theString: CFStringRef,
        buffer: *mut ::std::os::raw::c_char,
        buffer_size: CFIndex,
        encoding: CFStringEncoding,
    ) -> bool;
    fn CFStringGetCStringPtr(
        theString: CFStringRef,
        encoding: CFStringEncoding,
    ) -> *const ::std::os::raw::c_char;
    fn CFStringCreateWithCString(
        alloc: CFRef,
        source: *const ::std::os::raw::c_char,
        encoding: CFStringEncoding,
    ) -> CFStringRef;

    fn CFStringCompare(
        theString1: CFStringRef,
        theString2: CFStringRef,
        compareOptions: CFStringCompareFlags,
    ) -> CFComparisonResult;
    fn CFArrayRemoveValueAtIndex(theArray: CFMutableArrayRef, idx: CFIndex);

}
/// Address of Apple's immutable callback structure, not a copy or fake global.
pub fn type_array_callbacks() -> *const CFArrayCallBacks {
    match API.get_or_init(|| unsafe { Api::load() }) {
        Ok(api) => api.type_array_callbacks as *const CFArrayCallBacks,
        Err(_) => std::ptr::null(),
    }
}
/// Load the CFStringRef value stored in Apple's exported pointer DATA symbol.
pub fn run_loop_default_mode() -> CFStringRef {
    match API.get_or_init(|| unsafe { Api::load() }) {
        Ok(api) => unsafe { *(api.run_loop_default_mode as *const CFStringRef) },
        Err(_) => std::ptr::null_mut(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_table_native_data_and_string_roundtrip() {
        ensure_loaded().unwrap();
        let api = API.get().unwrap().as_ref().unwrap();
        assert_eq!(type_array_callbacks() as usize, api.type_array_callbacks);
        assert!(!run_loop_default_mode().is_null());
        let run_loop = unsafe { CFRunLoopGetCurrent() };
        assert!(!run_loop.is_null());
        assert!(unsafe { CFRunLoopIsWaiting(run_loop) } <= 1);
        assert!(
            unsafe { symbol(api._handle as *mut c_void, b"ParadoxMissingCFSymbol\0") }.is_err()
        );
        unsafe {
            let array = CFArrayCreateMutable(kCFAllocatorDefault, 0, type_array_callbacks());
            assert!(!array.is_null());
            let value = CFStringCreateWithCString(
                kCFAllocatorDefault,
                b"lazy native CF\0".as_ptr().cast(),
                kCFStringEncodingUTF8,
            );
            assert!(!value.is_null());
            CFArrayAppendValue(array, value);
            assert_eq!(CFArrayGetCount(array), 1);
            assert_eq!(CFArrayGetValueAtIndex(array, 0), value);
            let mut output = [0_i8; 64];
            assert!(CFStringGetCString(
                value,
                output.as_mut_ptr(),
                64,
                kCFStringEncodingUTF8
            ));
            assert_eq!(
                CStr::from_ptr(output.as_ptr()).to_bytes(),
                b"lazy native CF"
            );
            CFRelease(value);
            CFRelease(array);
        }
    }
}
