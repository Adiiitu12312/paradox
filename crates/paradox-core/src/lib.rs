//! Shared operations that delegate to the installed Git executable.
pub mod blob;
pub mod config;
pub mod git;
pub mod message;
pub mod repo;
pub mod store;

pub(crate) fn fault(point: &str) {
    #[cfg(feature = "fault-injection")]
    if std::env::var("PARADOX_TEST_CRASH_AT").as_deref() == Ok(point) {
        std::process::abort();
    }
    let _ = point;
}
