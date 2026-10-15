#![allow(non_upper_case_globals, non_camel_case_types)]

extern crate libc;

use std::ffi::CString;
use std::ptr;
use std::str;

pub type Boolean = ::std::os::raw::c_uchar;

pub type CFRef = *mut ::std::os::raw::c_void;

pub type CFIndex = ::std::os::raw::c_long;
pub type CFTimeInterval = f64;
pub type CFAbsoluteTime = CFTimeInterval;

#[doc(hidden)]
pub enum CFError {}

pub type CFAllocatorRef = CFRef;
pub type CFArrayRef = CFRef;
pub type CFMutableArrayRef = CFRef;
pub type CFURLRef = CFRef;
pub type CFErrorRef = *mut CFError;
pub type CFStringRef = CFRef;
pub type CFRunLoopRef = CFRef;

pub const NULL: CFRef = 0 as CFRef;
pub const NULL_REF_PTR: *mut CFRef = 0 as *mut CFRef;

pub type CFAllocatorRetainCallBack =
    extern "C" fn(*const ::std::os::raw::c_void) -> *const ::std::os::raw::c_void;
pub type CFAllocatorReleaseCallBack = extern "C" fn(*const ::std::os::raw::c_void);
pub type CFAllocatorCopyDescriptionCallBack =
    extern "C" fn(*const ::std::os::raw::c_void) -> *const CFStringRef;

pub type CFURLPathStyle = CFIndex;

pub const kCFAllocatorDefault: CFAllocatorRef = NULL;
pub const kCFURLPOSIXPathStyle: CFURLPathStyle = 0;
pub const kCFURLHFSPathStyle: CFURLPathStyle = 1;
pub const kCFURLWindowsPathStyle: CFURLPathStyle = 2;

pub const kCFStringEncodingUTF8: CFStringEncoding = 0x08000100;
pub type CFStringEncoding = u32;

pub const kCFCompareEqualTo: CFIndex = 0;
pub type CFComparisonResult = CFIndex;

// MacOS uses Case Insensitive path
pub const kCFCompareCaseInsensitive: CFStringCompareFlags = 1;
pub type CFStringCompareFlags = ::std::os::raw::c_ulong;

pub type CFArrayRetainCallBack =
    extern "C" fn(CFAllocatorRef, *const ::std::os::raw::c_void) -> *const ::std::os::raw::c_void;
pub type CFArrayReleaseCallBack = extern "C" fn(CFAllocatorRef, *const ::std::os::raw::c_void);
pub type CFArrayCopyDescriptionCallBack =
    extern "C" fn(*const ::std::os::raw::c_void) -> CFStringRef;
pub type CFArrayEqualCallBack =
    extern "C" fn(*const ::std::os::raw::c_void, *const ::std::os::raw::c_void) -> Boolean;

#[repr(C)]
pub struct CFArrayCallBacks {
    version: CFIndex,
    retain: Option<CFArrayRetainCallBack>,
    release: Option<CFArrayReleaseCallBack>,
    cp: Option<CFArrayCopyDescriptionCallBack>,
    equal: Option<CFArrayEqualCallBack>,
}
//impl Clone for CFArrayCallBacks { }

#[path = "lazy_core_foundation.rs"]
mod lazy;
pub use lazy::*;

pub unsafe fn str_path_to_cfstring_ref(source: &str, err: &mut CFErrorRef) -> CFStringRef {
    let c_path = CString::new(source).unwrap();
    let c_len = libc::strlen(c_path.as_ptr());
    let mut url = CFURLCreateFromFileSystemRepresentation(
        kCFAllocatorDefault,
        c_path.as_ptr(),
        c_len as CFIndex,
        false,
    );
    if url.is_null() {
        return ptr::null_mut();
    }

    let mut placeholder = CFURLCopyAbsoluteURL(url);
    CFRelease(url);
    if placeholder.is_null() {
        return ptr::null_mut();
    }

    let mut imaginary: CFRef = ptr::null_mut();

    while !CFURLResourceIsReachable(placeholder, ptr::null_mut()) {
        if imaginary.is_null() {
            imaginary = CFArrayCreateMutable(kCFAllocatorDefault, 0, type_array_callbacks());
            if imaginary.is_null() {
                CFRelease(placeholder);
                return ptr::null_mut();
            }
        }

        let child = CFURLCopyLastPathComponent(placeholder);
        CFArrayInsertValueAtIndex(imaginary, 0, child);
        CFRelease(child);

        url = CFURLCreateCopyDeletingLastPathComponent(kCFAllocatorDefault, placeholder);
        CFRelease(placeholder);
        placeholder = url;
    }

    url = CFURLCreateFileReferenceURL(kCFAllocatorDefault, placeholder, err);
    CFRelease(placeholder);
    if url.is_null() {
        if !imaginary.is_null() {
            CFRelease(imaginary);
        }
        return ptr::null_mut();
    }

    placeholder = CFURLCreateFilePathURL(kCFAllocatorDefault, url, err);
    CFRelease(url);
    if placeholder.is_null() {
        if !imaginary.is_null() {
            CFRelease(imaginary);
        }
        return ptr::null_mut();
    }

    if !imaginary.is_null() {
        let mut count = 0;
        while count < CFArrayGetCount(imaginary) {
            let component = CFArrayGetValueAtIndex(imaginary, count);
            url = CFURLCreateCopyAppendingPathComponent(
                kCFAllocatorDefault,
                placeholder,
                component,
                false,
            );
            CFRelease(placeholder);
            if url.is_null() {
                CFRelease(imaginary);
                return ptr::null_mut();
            }
            placeholder = url;
            count += 1;
        }
        CFRelease(imaginary);
    }

    let cf_path = CFURLCopyFileSystemPath(placeholder, kCFURLPOSIXPathStyle);
    CFRelease(placeholder);
    cf_path
}
