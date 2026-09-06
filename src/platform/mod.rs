//! Capture, injection, and the shared machinery between them.
//!
//! `engine` holds the listener state machine, written once and generic over the
//! [`engine::Platform`] each OS implements. The three OS modules hold only what
//! is genuinely their own: how keystrokes arrive, how a replacement is put on
//! screen, and how the process starts up.
//!
//! `textkeys` is the part macOS and Windows share — both capture `rdev::Key`
//! and both insert corrections as text — so that the answers they give the
//! engine are single-sourced even though the two remain separate platforms with
//! separate injection.

pub mod engine;

/// Start workers only after Linux has forked: other threads do not survive fork.
fn start_background_tasks() {
    crate::complete::spawn_watcher();
    crate::layout::spawn_watcher();
    crate::personal::init();
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod textkeys;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod tray;
