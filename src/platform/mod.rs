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

/// Friendly label plus the exact identifier used by the exclusion engine.
pub fn active_application() -> Option<(String, String)> {
    #[cfg(target_os = "macos")]
    {
        use cocoa::base::{id, nil};
        use cocoa::foundation::NSAutoreleasePool;
        use objc::{class, msg_send, sel, sel_impl};
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
            let app: id = msg_send![workspace, frontmostApplication];
            let result = (|| {
                if app == nil {
                    return None;
                }
                let pid: i32 = msg_send![app, processIdentifier];
                if pid == std::process::id() as i32 {
                    return None;
                }
                let name: id = msg_send![app, localizedName];
                let bundle: id = msg_send![app, bundleIdentifier];
                let string = |value: id| -> Option<String> {
                    if value == nil {
                        return None;
                    }
                    let ptr: *const std::os::raw::c_char = msg_send![value, UTF8String];
                    if ptr.is_null() {
                        return None;
                    }
                    std::ffi::CStr::from_ptr(ptr)
                        .to_str()
                        .ok()
                        .map(str::to_string)
                };
                Some((string(name)?, string(bundle)?))
            })();
            let _: () = msg_send![pool, drain];
            result
        }
    }
    #[cfg(target_os = "windows")]
    {
        use engine::Platform;
        let focus = windows::Windows::focus()?;
        let mut pid = 0;
        unsafe {
            winapi::um::winuser::GetWindowThreadProcessId(focus as _, &mut pid);
        }
        if pid == std::process::id() {
            return None;
        }
        let id = windows::Windows::app_id(&focus)?;
        Some((id.trim_end_matches(".exe").to_string(), id))
    }
    #[cfg(target_os = "linux")]
    {
        if !crate::layout::focus_supported() {
            return None;
        }
        let id = crate::layout::focused_target()?.app?;
        if id.eq_ignore_ascii_case("recast") {
            return None;
        }
        Some((id.clone(), id))
    }
}

/// Shared status wording for live controls. It never reads the focused text.
pub fn status(control: &crate::types::AppControl) -> String {
    #[cfg(target_os = "macos")]
    return status_for::<macos::Mac>(control);
    #[cfg(target_os = "windows")]
    return status_for::<windows::Windows>(control);
    #[cfg(target_os = "linux")]
    return status_for::<linux::Linux>(control);
}

fn status_for<P: engine::Platform>(control: &crate::types::AppControl) -> String {
    let focus = P::focus();
    let app = focus.as_ref().and_then(P::app_id);
    let mode =
        control.effective_app_mode(app.as_deref(), focus.as_ref().is_some_and(P::is_own_focus));
    if !control.is_switched_on() {
        return "Disabled — enable correction to resume.".into();
    }
    if let Some(left) = control.pause_remaining() {
        return format!(
            "Paused — {} min left. Choose Resume to continue now.",
            left.as_secs() / 60 + 1
        );
    }
    if !control
        .listener_ready
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return "Keyboard listener unavailable — check permissions and connected keyboards, then reopen ReCast if needed.".into();
    }
    if !P::input_allowed() {
        return "Secure Input — correction resumes when secure entry ends.".into();
    }
    if let Some(id) = control.paused_app() {
        return format!("Paused in {id} — switch to another application or choose Resume in {id}.");
    }
    if mode.is_none() || (P::requires_focus() && focus.is_none()) {
        return "Cannot identify this application — switch to a text field and check focus/accessibility support. Configured app restrictions stay enforced.".into();
    }
    match mode.unwrap() {
        crate::config::AppMode::Off => {
            "Excluded application — choose Full correction or Layout only in Application modes."
                .into()
        }
        _ if P::current_layout().is_none() => "Keyboard layout unavailable — enable English and Hebrew keyboards and check the layout backend.".into(),
        crate::config::AppMode::LayoutOnly => {
            "Ready — layout only; spelling, abbreviations, and completion are off.".into()
        }
        crate::config::AppMode::Full => "Ready — correction follows your enabled settings.".into(),
    }
}
