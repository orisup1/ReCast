use std::process;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

// Only the macOS menubar title uses the banner (Windows shows a tooltip).
#[cfg(target_os = "macos")]
use crate::banner;
use crate::types::AppControl;

/// How often the menu's counters are refreshed while idle.
const STATUS_REFRESH: Duration = Duration::from_millis(750);

/// How long "Pause" pauses for. Long enough to get through the thing that
/// prompted it (a terminal session, a password-heavy form, dictating a name),
/// short enough that forgetting to switch it back on isn't a silent week
/// without corrections — which is the failure mode of a plain Disable.
const PAUSE_LENGTH: Duration = Duration::from_secs(30 * 60);

/// How many recent corrections the menu lists.
const RECENT_SLOTS: usize = 5;

/// Run the menubar (macOS) / tray (Windows) on the calling thread.
///
/// Must be invoked from the main thread — `tao` creates the platform event
/// loop here (NSApp on macOS, Win32 message pump on Windows) and both require
/// the main thread.
#[allow(unused_assignments)]
pub fn run(control: Arc<AppControl>) {
    let mut builder = EventLoopBuilder::new();
    #[allow(unused_mut)]
    let mut event_loop = builder.build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        // Accessory keeps the app out of the Dock and the Cmd-Tab switcher —
        // it lives only in the menubar. Must be set before `run()`.
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }

    let menu = Menu::new();
    // Informational row: what it has done, and how much of that was thrown
    // back at it (see `status_label`).
    let status_item = MenuItem::new(status_label(&control), false, None);
    let toggle_item = MenuItem::new(toggle_label(control.is_switched_on()), true, None);
    let pause_item = MenuItem::new(pause_label(None), true, None);
    let sep = MenuItem::new("", false, None);

    // The recent-corrections list. Silent text replacement is the whole
    // premise of this app, so "what did it just change?" needs an answer that
    // isn't a counter — and the answer is only useful if you can act on it,
    // which is what clicking one does: that word goes into `ignore.txt` and is
    // never corrected again.
    //
    // The rows are added as corrections happen rather than sitting there
    // empty: five blank lines in a native menu read as something broken, and
    // the submenu stays greyed out until there is a first one to show.
    let recent_menu = Submenu::new("Recent — click one to stop correcting it", false);
    let recent_items: Vec<MenuItem> = (0..RECENT_SLOTS)
        .map(|_| MenuItem::new("", true, None))
        .collect();
    // How many of `recent_items` have been put into the submenu so far.
    let mut recent_shown = 0usize;
    let recent_ids: Vec<_> = recent_items.iter().map(|i| i.id().clone()).collect();
    // What each slot currently refers to, so a click knows which word it is
    // about. Rebuilt with the labels on every refresh.
    let mut recent_words: Vec<String> = vec![String::new(); RECENT_SLOTS];

    let reload_item = MenuItem::new("Reload lists", true, None);
    // Only offered where it is wired up; elsewhere the item would be a
    // checkbox that does nothing.
    let autostart_item = crate::prefs::autostart_enabled()
        .map(|on| CheckMenuItem::new("Start at login", true, on, None));
    let about_item = MenuItem::new("About ReCast", true, None);
    let quit_item = MenuItem::new("Quit", true, None);

    menu.append(&status_item).expect("append status");
    menu.append(&toggle_item).expect("append toggle");
    menu.append(&pause_item).expect("append pause");
    menu.append(&sep).expect("append separator");
    menu.append(&recent_menu).expect("append recent");
    menu.append(&reload_item).expect("append reload");
    if let Some(item) = &autostart_item {
        menu.append(item).expect("append autostart");
    }
    menu.append(&about_item).expect("append about");
    menu.append(&quit_item).expect("append quit");

    let toggle_id = toggle_item.id().clone();
    let pause_id = pause_item.id().clone();
    let reload_id = reload_item.id().clone();
    let autostart_id = autostart_item.as_ref().map(|i| i.id().clone());
    let about_id = about_item.id().clone();
    let quit_id = quit_item.id().clone();
    let menu_channel = MenuEvent::receiver();

    // Track what has been rendered so we only rewrite labels when they
    // change, avoiding needless native menu churn on every timer wake.
    let mut last_status = status_label(&control);
    let mut last_pause: Option<u64> = None;
    let mut last_recent: Vec<String> = vec![String::new(); RECENT_SLOTS];

    // tray-icon (macOS) requires that the TrayIcon be created after the
    // NSApplication has finished launching — i.e. inside the run loop, on
    // StartCause::Init. `take()` on the Option ensures we only build once.
    // `_tray` is held by the closure to keep the icon alive for the program's
    // lifetime; we never read it back after construction.
    let mut pending_menu: Option<Menu> = Some(menu);
    let mut _tray: Option<TrayIcon> = None;

    event_loop.run(move |event, _target, control_flow| {
        // Wake periodically to refresh the fixed-word counter; menu/tray
        // events still wake us immediately in between.
        *control_flow = ControlFlow::WaitUntil(Instant::now() + STATUS_REFRESH);

        // Keep the counters in sync with the listener's running totals.
        let status = status_label(&control);
        if status != last_status {
            status_item.set_text(&status);
            last_status = status;
        }

        // A pause counts itself down in the menu, and puts the label back when
        // it runs out — a pause you can't see the end of is a disable.
        let remaining = control.pause_remaining();
        let minutes = remaining.map(|left| left.as_secs() / 60);
        if minutes != last_pause {
            last_pause = minutes;
            pause_item.set_text(pause_label(remaining));
        }

        // Refresh the recent-corrections slots, adding rows as the history
        // fills up. It only ever grows (to `RECENT_SLOTS`), so this appends a
        // handful of times over the life of the process and never churns.
        let history = control.history();
        while recent_shown < history.len().min(RECENT_SLOTS) {
            recent_menu
                .append(&recent_items[recent_shown])
                .expect("append recent");
            recent_shown += 1;
            recent_menu.set_enabled(true);
        }
        for (slot, item) in recent_items.iter().enumerate().take(recent_shown) {
            let label = history.get(slot).map(recent_label).unwrap_or_default();
            if label != last_recent[slot] {
                item.set_text(&label);
                last_recent[slot] = label;
            }
            recent_words[slot] = history.get(slot).map(|c| c.from.clone()).unwrap_or_default();
        }

        if let Event::NewEvents(StartCause::Init) = event {
            if let Some(menu) = pending_menu.take() {
                let icon = app_icon();
                #[allow(unused_mut)]
                let mut tray_builder = TrayIconBuilder::new()
                    .with_menu(Box::new(menu))
                    .with_tooltip(tooltip(&control))
                    .with_icon(icon);
#[cfg(target_os = "macos")]
{
  let title = if banner::ran_from_terminal() {
    menubar_banner()
  } else {
    "ReCast".to_string()
  };
  tray_builder = tray_builder.with_title(&title);
}
                _tray = Some(tray_builder.build().expect("tray build"));
            }
        }

        while let Ok(event) = menu_channel.try_recv() {
            if event.id == toggle_id {
                let new_enabled = !control.is_switched_on();
                control.set_enabled(new_enabled);
                toggle_item.set_text(toggle_label(new_enabled));
                // The hover text carries the same state as the menu, so it is
                // refreshed here rather than waiting for the next timer wake.
                let _ = _tray.as_ref().map(|t| t.set_tooltip(Some(tooltip(&control))));
            } else if event.id == pause_id {
                // The same item ends the pause it started: while one is
                // running the row reads "Resume", so this is one control with
                // two states rather than two rows that contradict each other.
                if control.pause_remaining().is_some() {
                    control.resume();
                } else {
                    control.pause_for(PAUSE_LENGTH);
                }
                pause_item.set_text(pause_label(control.pause_remaining()));
                let _ = _tray.as_ref().map(|t| t.set_tooltip(Some(tooltip(&control))));
            } else if event.id == reload_id {
                // The watcher picks edits up on its own within a couple of
                // seconds; this is for the user who has just saved the file
                // and wants to know *now* that it took.
                crate::complete::reload_user_files();
                let (abbrevs, ignored) = crate::complete::list_counts();
                crate::notify::notify(
                    "ReCast reloaded your lists",
                    &format!("{abbrevs} abbreviation(s), {ignored} ignored word(s)"),
                );
            } else if Some(&event.id) == autostart_id.as_ref() {
                if let Some(item) = &autostart_item {
                    // The checkbox has already flipped itself; if the OS
                    // refuses the change, put it back rather than show a state
                    // that isn't true.
                    let wanted = item.is_checked();
                    if !crate::prefs::set_autostart(wanted) {
                        item.set_checked(!wanted);
                    }
                }
            } else if recent_ids.contains(&event.id) {
                // Clicking a correction is how you say "not this word, ever".
                if let Some(slot) = recent_ids.iter().position(|id| *id == event.id) {
                    let word = recent_words[slot].clone();
                    if !word.is_empty() {
                        crate::complete::ignore_word(&word);
                        crate::notify::notify(
                            "ReCast will leave that word alone",
                            &format!("\"{word}\" is now listed in ignore.txt."),
                        );
                    }
                }
            } else if event.id == about_id {
                #[cfg(target_os = "macos")]
                {
                    // NSAlert only: configured with setMessageText: /
                    // setInformativeText: and shown with runModal, and every
                    // string built as a real NSString. Sending it a selector it
                    // does not implement, or a Rust &str where NSString* is
                    // expected, is an Objective-C exception — which aborts the
                    // whole process rather than failing the click.
                    use cocoa::appkit::NSApp;
                    use cocoa::base::{id, nil, YES};
                    use cocoa::foundation::NSString;
                    use objc::{class, msg_send};
                    use objc::sel;
                    use objc::sel_impl;

                    let info = format!(
                        "Layout mistake fixer for bilingual typing.\n\n\
                         Version {}\n\n\
                         Created by Ori Supino\n\
                         © 2026 Ori Supino",
                        env!("CARGO_PKG_VERSION")
                    );

                    unsafe {
                        // Accessory apps have no key window, so pull ReCast to the
                        // front or the alert can appear buried behind other apps.
                        let _: () = msg_send![NSApp(), activateIgnoringOtherApps: YES];

                        let alert: id = msg_send![class!(NSAlert), new];
                        let title = NSString::alloc(nil).init_str("ReCast");
                        let body = NSString::alloc(nil).init_str(&info);
                        let ok = NSString::alloc(nil).init_str("OK");
                        let _: () = msg_send![alert, setMessageText: title];
                        let _: () = msg_send![alert, setInformativeText: body];
                        let _: id = msg_send![alert, addButtonWithTitle: ok];
                        let _: i64 = msg_send![alert, runModal];
                        // The three NSStrings are alloc/init-owned; leaking them
                        // per (rare) About click is negligible and keeps this off
                        // the manual-release path.
                    }
                }
                #[cfg(target_os = "windows")]
                {
                    use winapi::um::winuser::{
                        MessageBoxW, MB_OK, MB_ICONINFORMATION, MB_SETFOREGROUND,
                    };
                    use std::ffi::OsString;
                    use std::os::windows::ffi::OsStrExt;
                    let body = format!(
                        "Layout mistake fixer for bilingual typing.\n\nVersion {}\n\nCreated by Ori Supino\n© 2026 Ori Supino",
                        env!("CARGO_PKG_VERSION")
                    );
                    let wide: Vec<u16> =
                        OsString::from(body).encode_wide().chain(std::iter::once(0)).collect();
                    let caption: Vec<u16> = OsString::from("About ReCast")
                        .encode_wide().chain(std::iter::once(0)).collect();
                    // A tray app has no foreground window, so MB_SETFOREGROUND is
                    // needed for the dialog to reliably surface above other windows.
                    unsafe {
                        MessageBoxW(
                            std::ptr::null_mut(),
                            wide.as_ptr(),
                            caption.as_ptr(),
                            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
                        );
                    }
                }
            } else if event.id == quit_id {
                // Drop the tray icon first so Windows removes it from the
                // notification area immediately. process::exit skips destructors,
                // which would otherwise leave a ghost icon behind until the user
                // moves the mouse over it.
                let _ = _tray.take();
                process::exit(0);
            }
        }
    });
}

fn toggle_label(enabled: bool) -> &'static str {
    if enabled { "Disable" } else { "Enable" }
}

/// The counter row: what stuck, what was taken back, and — once enough has
/// been taken back to mean something — what to do about it.
///
/// The undo tally is here rather than hidden because it is the only number
/// that says whether the speller is set where this user wants it: corrections
/// nobody undoes are invisible by design, so a raw "fixed" count can't
/// distinguish working well from working badly.
fn status_label(control: &AppControl) -> String {
    let mut label = format!("Fixed: {}", control.fixed_count());
    let undone = control.undo_count();
    if undone > 0 {
        label.push_str(&format!(" · {undone} taken back"));
    }
    if let Some(hint) = control.tighten_hint() {
        label.push_str(&format!(" — {hint}"));
    }
    label
}

fn pause_label(remaining: Option<Duration>) -> String {
    match remaining {
        // Rounded up: a pause with "0 min left" showing for a whole minute
        // reads as broken.
        Some(left) => format!("Resume (paused, {} min left)", left.as_secs() / 60 + 1),
        None => format!("Pause for {} minutes", PAUSE_LENGTH.as_secs() / 60),
    }
}

/// One line of the recent-corrections list. Undone ones stay on it, marked:
/// "it changed this and I put it back" is exactly what someone is looking for
/// when they go looking.
fn recent_label(correction: &crate::types::Correction) -> String {
    format!(
        "{}{} → {}  ({})",
        if correction.undone { "↩ " } else { "" },
        correction.from,
        correction.to,
        correction.kind.tag()
    )
}

fn tooltip(control: &AppControl) -> String {
    let state = match control.pause_remaining() {
        Some(left) => format!("Paused, {} min left", left.as_secs() / 60 + 1),
        None if control.is_switched_on() => "Enabled".to_string(),
        None => "Disabled".to_string(),
    };
    format!("ReCast - {state} - {} fixed", control.fixed_count())
}

// Compose a single-line menubar banner: a compact half-block icon strip
// followed by a short label. macOS menubar titles are single-line only and
// ignore ANSI escape sequences, so we rely on half-block characters plus
// color for truecolor terminals; plain fallback strips to "ReCast vX".
//
// macOS-only: it is the only platform whose tray item carries an inline title
// (Windows uses a hover tooltip), so gating it avoids a dead-code warning there.
#[cfg(target_os = "macos")]
fn menubar_banner() -> String {
  if std::env::var_os("NO_COLOR").is_some() {
    format!("ReCast v{}", env!("CARGO_PKG_VERSION"))
  } else {
    let depth = banner::ColorDepth::True;
    let mut row = banner::logo_rows_compact(depth);
    if !row.is_empty() { row.push(' '); }
    row.push_str("\x1b[38;5;39mReCast\x1b[0m ");
    row.push_str(&format!("\x1b[2mv{}\x1b[0m", env!("CARGO_PKG_VERSION")));
    row
  }
}

const ICON_RGBA: &[u8] = include_bytes!("../../assets/tray-icon.rgba");
const ICON_SIZE: u32 = 32;

fn app_icon() -> Icon {
    Icon::from_rgba(ICON_RGBA.to_vec(), ICON_SIZE, ICON_SIZE).expect("icon build")
}
