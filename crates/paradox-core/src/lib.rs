//! Shared operations that delegate to the installed Git executable.
pub mod blob;
pub mod commit;
pub mod config;
pub mod git;
pub mod history;
pub mod message;
pub mod repo;
pub mod setup;
pub mod store;
pub mod tracking;
pub mod ui_git;
pub mod ui_actions;
pub mod watch;
#[cfg(target_os = "macos")]
mod watch_signal;

pub(crate) fn fault(point: &str) {
    #[cfg(feature = "fault-injection")]
    if std::env::var("PARADOX_TEST_CRASH_AT").as_deref() == Ok(point) {
        std::process::abort();
    }
    let _ = point;
}
