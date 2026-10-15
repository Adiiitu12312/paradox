//! Authenticated, session-only browser interface bound exclusively to loopback.
mod assets;
mod server;
pub use server::{UiOptions, UiReady, serve};
