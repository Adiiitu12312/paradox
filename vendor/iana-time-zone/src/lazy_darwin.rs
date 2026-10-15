// Diagnostic-only late-bound CoreFoundation calls. Keep core-foundation-sys
// ABI types without its eager framework link; the timezone algorithm is intact.
use core_foundation_sys::base::{Boolean, CFIndex, CFRange, CFTypeRef};
use core_foundation_sys::string::{CFStringEncoding, CFStringRef};
use core_foundation_sys::timezone::CFTimeZoneRef;
use std::ffi::{c_void, CStr};
use std::os::raw::c_char;
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
impl Unavailable for isize {
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
            $($name: unsafe extern "C" fn($($kind),*) $(-> $result)?,)+
        }
        impl Api {
            unsafe fn load() -> Result<Self, String> {
                let handle = unsafe { libc::dlopen(FRAMEWORK.as_ptr().cast(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
                if handle.is_null() { return Err(loader_error("Cannot load CoreFoundation framework")); }
                // Retain handle even on partial failure; no partial table escapes.
                Ok(Self { _handle: handle as usize,
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
    fn CFRelease(value: CFTypeRef);
    fn CFTimeZoneResetSystem();
    fn CFTimeZoneCopySystem() -> CFTimeZoneRef;
    fn CFTimeZoneGetName(zone: CFTimeZoneRef) -> CFStringRef;
    fn CFStringGetCStringPtr(value: CFStringRef, encoding: CFStringEncoding) -> *const c_char;
    fn CFStringGetLength(value: CFStringRef) -> CFIndex;
    fn CFStringGetBytes(value: CFStringRef, range: CFRange, encoding: CFStringEncoding, loss: u8, external: Boolean, buffer: *mut u8, max: CFIndex, used: *mut CFIndex) -> CFIndex;
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loads_all_timezone_symbols_and_reports_missing() {
        ensure_loaded().unwrap();
        let api = API.get().unwrap().as_ref().unwrap();
        assert!(unsafe {
            symbol(
                api._handle as *mut c_void,
                b"ParadoxMissingTimezoneSymbol\0",
            )
        }
        .is_err());
    }
}
