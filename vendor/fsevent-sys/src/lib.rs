#![cfg(target_os = "macos")]
#![allow(clippy::unreadable_literal)]

pub mod core_foundation;
mod fsevent;

pub use fsevent::*;
