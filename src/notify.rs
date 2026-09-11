//! The one time ReCast interrupts you.
//!
//! Everything this program does happens inside other people's text fields, with
//! no window of its own, which makes its two gestures (double-tap Ctrl to take
//! a correction back, tap Right Shift to finish a word) invisible: they are in
//! the README, and the README is not where anyone is when their word is
//! silently rewritten for the first time. So the first correction — ever, not
//! per run — says so once, and then never again.
//!
//! Once is the whole design. A notification per correction would be worse than
//! no notification at all, and a hint the user has already read is noise, so
//! the marker file that records "they have seen this" lives in the config
//! directory and outlives the process.

use std::sync::atomic::{AtomicBool, Ordering};

pub const SHORTCUTS: &str = "Undo: tap Ctrl twice within half a second, immediately after a correction. Typing anything else or moving the cursor ends the undo opportunity.\n\nCompletion: tap Right Shift mid-word. Tap again to cycle suggestions and eventually restore your original prefix. Holding Shift to capitalize is unaffected.\n\nOne undo leaves that word alone for this session. Undoing it on two occasions remembers that preference across restarts.\n\nTo allow a word again: type the ignored word and its space, then double-tap Ctrl immediately. This removes its saved exception and may correct it.\n\nReCast processes typing locally. Recent corrections stay in memory; ignored and learned words are saved locally.";

/// Explicitly requested help may use a dialog; typing notifications must not.
#[cfg(target_os = "macos")]
pub fn dialog(title: &str, body: &str, buttons: &[&str]) -> usize {
    dialog_with_destination(title, body, buttons, None)
}

#[cfg(target_os = "macos")]
pub enum DialogDestination<'a> {
    Settings(&'a str),
    Reveal(&'a std::path::Path),
}

/// Hand off focus only after the next setup dialog has appeared. Opening Settings
/// before runModal would immediately cover it with the new ReCast dialog.
#[cfg(target_os = "macos")]
pub fn dialog_with_destination(
    title: &str,
    body: &str,
    buttons: &[&str],
    destination: Option<DialogDestination<'_>>,
) -> usize {
    use cocoa::base::{id, nil, YES};
    use cocoa::foundation::{NSAutoreleasePool, NSString};
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let app: id = msg_send![class!(NSApplication), sharedApplication];
        let _: bool = msg_send![app, setActivationPolicy: 1isize];
        let _: () = msg_send![app, activateIgnoringOtherApps: YES];
        let alert: id = msg_send![class!(NSAlert), new];
        let heading = NSString::alloc(nil).init_str(title);
        let text = NSString::alloc(nil).init_str(body);
        let _: () = msg_send![alert, setMessageText: heading];
        let _: () = msg_send![alert, setInformativeText: text];
        for (index, button) in buttons.iter().enumerate() {
            let label = NSString::alloc(nil).init_str(button);
            let button: id = msg_send![alert, addButtonWithTitle: label];
            let _: () = msg_send![button, setTag: (1000 + index) as isize];
            let _: () = msg_send![label, release];
        }
        if let Some(destination) = destination {
            let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
            let (selector, argument) = match destination {
                DialogDestination::Settings(url) => {
                    let text = NSString::alloc(nil).init_str(url);
                    let url: id = msg_send![class!(NSURL), URLWithString: text];
                    let _: () = msg_send![text, release];
                    (sel!(openURL:), url)
                }
                DialogDestination::Reveal(path) => {
                    let text = NSString::alloc(nil).init_str(&path.to_string_lossy());
                    let url: id = msg_send![class!(NSURL), fileURLWithPath: text];
                    let urls: id = msg_send![class!(NSArray), arrayWithObject: url];
                    let _: () = msg_send![text, release];
                    (sel!(activateFileViewerSelectingURLs:), urls)
                }
            };
            let mode = NSString::alloc(nil).init_str("NSModalPanelRunLoopMode");
            let modes: id = msg_send![class!(NSArray), arrayWithObject: mode];
            let _: () = msg_send![workspace, performSelector: selector withObject: argument afterDelay: 0.0f64 inModes: modes];
            let _: () = msg_send![mode, release];
        }
        let result: isize = msg_send![alert, runModal];
        let _: () = msg_send![heading, release];
        let _: () = msg_send![text, release];
        let _: () = msg_send![alert, release];
        let _: () = msg_send![pool, drain];
        (result - 1000).max(0) as usize
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn show_shortcuts() {
    #[cfg(target_os = "macos")]
    dialog("Typing shortcuts", SHORTCUTS, &["Done"]);
    #[cfg(target_os = "windows")]
    unsafe {
        use winapi::um::winuser::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
        let title: Vec<u16> = "Typing shortcuts".encode_utf16().chain(Some(0)).collect();
        let body: Vec<u16> = SHORTCUTS.encode_utf16().chain(Some(0)).collect();
        MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

/// Whether this run has already dealt with the hint — checked before the
/// filesystem is, so the steady state costs one relaxed atomic load per
/// correction rather than a `stat`.
static HANDLED: AtomicBool = AtomicBool::new(false);

/// Called on every correction; acts on the first one this user has ever had.
///
/// The work happens on a spawned thread because the caller is a keyboard event
/// callback: on macOS it runs inside the event tap, where blocking on a
/// subprocess would stall the keystroke itself and eventually have the OS
/// disable the tap out from under us.
/// Deliberately says nothing about *which* word. The user has just watched it
/// happen on screen, so the words add little — and a notification is a copy of
/// text that outlives the moment, sitting in a notification centre after the
/// window it came from is closed. On macOS a password field is already out of
/// reach (see `platform::macos::secure_input_active`), but there is no
/// equivalent signal on Linux or Windows, and this fires exactly once with no
/// way to know what it is about to quote.
pub fn first_correction_hint() {
    // The test suite records corrections by the dozen; none of them should
    // reach the user's desktop or write the marker into their config
    // directory, which would also cost them the hint for real.
    if cfg!(test) || HANDLED.swap(true, Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(|| {
        if crate::prefs::welcomed() {
            return;
        }
        crate::prefs::mark_welcomed();
        notify(
            "ReCast just corrected a word",
            "Ctrl twice immediately undoes a correction and skips that word this session. \
             Repeated undos remember it. Right Shift completes; tap again to cycle. \
             See Typing shortcuts for help.",
        );
    });
}

/// Show a desktop notification. Best-effort on every platform: a missing
/// notification daemon is not a reason to do anything else differently.
pub fn notify(title: &str, body: &str) {
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .args(["-a", "ReCast", title, body])
            .status();
    }
    #[cfg(target_os = "macos")]
    {
        // AppleScript string literals have no escape for a bare quote that
        // survives `-e`, so the text is stripped of the two characters that
        // could end the literal early rather than escaped into it.
        let clean = |s: &str| s.replace(['"', '\\'], "");
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(format!(
                "display notification \"{}\" with title \"{}\"",
                clean(body),
                clean(title)
            ))
            .status();
    }
    #[cfg(target_os = "windows")]
    {
        // The tray thread presents these without opening or focusing a window.
        if let Ok(mut pending) = WINDOWS_NOTICES.lock() {
            if pending.len() < 8 {
                pending.push_back((title.to_string(), body.to_string()));
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub static WINDOWS_NOTICES: std::sync::Mutex<std::collections::VecDeque<(String, String)>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());
