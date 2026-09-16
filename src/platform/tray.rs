use std::process;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tao::event::{Event, StartCause, WindowEvent};
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
    let health_menu = Submenu::new("Status", true);
    let health_detail = MenuItem::new("Starting keyboard listener…", false, None);
    health_menu
        .append(&health_detail)
        .expect("append status detail");
    let toggle_item = MenuItem::new(toggle_label(control.is_switched_on()), true, None);
    let pause_item = MenuItem::new(pause_label(None), true, None);
    let app_pause_item = MenuItem::new("Pause in application (waiting for focus)", false, None);
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

    let settings_item = MenuItem::new("Open settings", true, None);
    let settings_menu = Submenu::new("Settings", true);
    let config = crate::config::Config::global();
    let spell_item =
        CheckMenuItem::new("Correct English spelling", true, config.spell_enabled, None);
    let complete_item = CheckMenuItem::new(
        "Word completion and abbreviations",
        true,
        config.complete_enabled,
        None,
    );
    let conservative_item = CheckMenuItem::new(
        "Conservative spelling (single-typo fixes)",
        true,
        config.spell_max_dist == 1,
        None,
    );
    settings_menu.append(&spell_item).expect("append spelling");
    settings_menu
        .append(&complete_item)
        .expect("append completion");
    settings_menu
        .append(&conservative_item)
        .expect("append conservative");
    let undo_menu = Submenu::new("Extra undo shortcut", true);
    let undo_items: Vec<_> = [
        ("none", "Double-tap Ctrl only"),
        ("left_ctrl", "Also single-tap Left Ctrl"),
        ("right_ctrl", "Also single-tap Right Ctrl"),
    ]
    .into_iter()
    .map(|(value, label)| {
        let item = CheckMenuItem::new(label, true, config.undo_shortcut == value, None);
        undo_menu.append(&item).expect("append undo choice");
        (item, value)
    })
    .collect();
    settings_menu.append(&undo_menu).expect("append undo menu");
    settings_item.set_text("Advanced settings… (file edits need restart)");
    settings_menu
        .append(&settings_item)
        .expect("append advanced settings");
    let apps_menu = Submenu::new("Application modes", true);
    let exclude_item = MenuItem::new("Switch to an app first", false, None);
    apps_menu.append(&exclude_item).expect("append exclude app");
    let mode_items: Vec<_> = crate::config::AppMode::ALL
        .into_iter()
        .map(|mode| {
            let item = CheckMenuItem::new(mode.label(), false, false, None);
            apps_menu.append(&item).expect("append mode");
            (item, mode)
        })
        .collect();
    let apps_hint = MenuItem::new(
        "Click a saved app below to restore Full correction",
        false,
        None,
    );
    apps_menu.append(&apps_hint).expect("append apps hint");
    let mut excluded_items: Vec<(MenuItem, String)> = Vec::new();
    let mut last_app: Option<(String, String)> = None;
    let shortcuts_item = MenuItem::new("Typing shortcuts…", true, None);
    let practice_item = MenuItem::new("Practice correction and undo…", true, None);
    let ignored_item = MenuItem::new("Open ignored words", true, None);
    let reload_item = MenuItem::new("Reload lists", true, None);
    // Only offered where it is wired up; elsewhere the item would be a
    // checkbox that does nothing.
    let autostart_item = crate::prefs::autostart_enabled()
        .map(|on| CheckMenuItem::new("Start at login", true, on, None));
    let about_item = MenuItem::new("About ReCast", true, None);
    let quit_item = MenuItem::new("Quit", true, None);

    menu.append(&status_item).expect("append status");
    menu.append(&health_menu).expect("append health");
    menu.append(&toggle_item).expect("append toggle");
    menu.append(&pause_item).expect("append pause");
    menu.append(&app_pause_item).expect("append app pause");
    menu.append(&sep).expect("append separator");
    menu.append(&recent_menu).expect("append recent");
    menu.append(&settings_menu).expect("append settings");
    menu.append(&apps_menu).expect("append apps");
    menu.append(&shortcuts_item).expect("append shortcuts");
    menu.append(&practice_item).expect("append practice");
    menu.append(&ignored_item).expect("append ignored words");
    menu.append(&reload_item).expect("append reload");
    if let Some(item) = &autostart_item {
        menu.append(item).expect("append autostart");
    }
    menu.append(&about_item).expect("append about");
    menu.append(&quit_item).expect("append quit");

    let toggle_id = toggle_item.id().clone();
    let status_id = status_item.id().clone();
    let pause_id = pause_item.id().clone();
    let app_pause_id = app_pause_item.id().clone();
    let settings_id = settings_item.id().clone();
    let spell_id = spell_item.id().clone();
    let complete_id = complete_item.id().clone();
    let conservative_id = conservative_item.id().clone();
    let shortcuts_id = shortcuts_item.id().clone();
    let practice_id = practice_item.id().clone();
    let ignored_id = ignored_item.id().clone();
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
    let mut health = String::new();
    let mut last_health_check = Instant::now() - STATUS_REFRESH;
    let mut practice_window: Option<crate::practice::native::Window> = None;
    let mut practice_offered = false;
    #[cfg(target_os = "windows")]
    let mut balloon_until: Option<Instant> = None;

    event_loop.run(move |event, target, control_flow| {
        // Wake periodically to refresh the fixed-word counter; menu/tray
        // events still wake us immediately in between.
        *control_flow = ControlFlow::WaitUntil(Instant::now() + STATUS_REFRESH);

        if let Some(app) = super::active_application() {
            last_app = Some(app);
        }
        let mut excluded = crate::types::lock_forgiving(&control.excluded_apps).clone();
        excluded.extend(crate::types::lock_forgiving(&control.layout_only_apps).iter().cloned());
        excluded.sort();
        excluded.dedup();
        if last_health_check.elapsed() >= STATUS_REFRESH {
            health = super::status(&control);
            let (state, detail) = health.split_once(" — ").unwrap_or((&health, ""));
            health_menu.set_text(format!("Status: {state}"));
            health_detail.set_text(detail);
            if let Some(tray) = &_tray { let _ = tray.set_tooltip(Some(format!("ReCast — {state}"))); }
            last_health_check = Instant::now();
        }
        if let Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. } = &event {
            if practice_window.as_ref().is_some_and(|practice| practice.window.id() == *window_id) {
                control.practice_open.store(false, std::sync::atomic::Ordering::Relaxed);
                practice_window = None;
            }
        }
        if let Some(practice) = &mut practice_window { practice.update(&control, &health); }
        if let Some(id) = control.paused_app() {
            app_pause_item.set_text(format!("Resume in {id}"));
            app_pause_item.set_enabled(true);
        } else if let Some((name, _)) = &last_app {
            app_pause_item.set_text(format!("Pause in {name} until I switch away"));
            app_pause_item.set_enabled(true);
        }
        if let Some((name, id)) = &last_app {
            exclude_item.set_text(format!("{name} ({id})"));
            for (item, mode) in &mode_items {
                item.set_enabled(true);
                item.set_checked(control.app_mode(Some(id)) == Some(*mode));
            }
        }
        if excluded_items.iter().map(|(_, id)| id).ne(excluded.iter()) {
            for (item, _) in excluded_items.drain(..) {
                let _ = apps_menu.remove(&item);
            }
            for id in &excluded {
                let item = MenuItem::new(id, true, None);
                apps_menu.append(&item).expect("append excluded application");
                excluded_items.push((item, id.clone()));
            }
        }
        for (item, id) in &excluded_items {
            item.set_text(format!("{id}: {} — restore Full", control.app_mode(Some(id)).unwrap().label()));
        }

        // Keep the counters in sync with the listener's running totals.
        status_item.set_enabled(control.tighten_hint().is_some() && crate::config::Config::global().spell_max_dist > 1);
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
        if !practice_offered && pending_menu.is_none() && control.listener_ready.load(std::sync::atomic::Ordering::Relaxed) {
            practice_offered = true;
            if crate::practice::first_run() {
                match crate::practice::native::Window::new(target, &control) {
                    Ok(window) => practice_window = Some(window),
                    Err(error) => crate::notify::notify("Could not open practice", &error),
                }
            }
        }

        #[cfg(target_os = "windows")]
        if let Some(tray) = &_tray {
            if balloon_until.is_some_and(|until| Instant::now() >= until) {
                windows_balloon(tray, None);
                balloon_until = None;
            }
            if balloon_until.is_none() {
                let notice = crate::notify::WINDOWS_NOTICES.lock().ok().and_then(|mut q| q.pop_front());
                if let Some((title, body)) = notice {
                    windows_balloon(tray, Some((&title, &body)));
                    balloon_until = Some(Instant::now() + Duration::from_secs(12));
                }
            }
        }

        while let Ok(event) = menu_channel.try_recv() {
            if event.id == status_id {
                if let Err(error) = crate::settings::set_live(&control, "spell_dist", "1") {
                    crate::notify::notify("Settings unchanged", &error);
                }
                conservative_item.set_checked(crate::config::Config::global().spell_max_dist == 1);
            } else if event.id == toggle_id {
                let new_enabled = !control.is_switched_on();
                control.set_enabled(new_enabled);
                toggle_item.set_text(toggle_label(new_enabled));
                // The hover text carries the same state as the menu, so it is
                // refreshed here rather than waiting for the next timer wake.
                let _ = _tray.as_ref().map(|t| t.set_tooltip(Some(tooltip(&control))));
            } else if event.id == app_pause_id {
                if control.paused_app().is_some() {
                    control.resume_app();
                } else if let Some((_, id)) = &last_app {
                    control.pause_in_app(id);
                }
                last_health_check = Instant::now() - STATUS_REFRESH;
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
            } else if event.id == practice_id {
                if let Some(practice) = &practice_window {
                    practice.focus();
                } else {
                    match crate::practice::native::Window::new(target, &control) {
                        Ok(window) => practice_window = Some(window),
                        Err(error) => crate::notify::notify("Could not open practice", &error),
                    }
                }
            } else if event.id == shortcuts_id {
                crate::notify::show_shortcuts();
            } else if event.id == spell_id || event.id == complete_id || event.id == conservative_id {
                let (key, value) = if event.id == spell_id {
                    ("spell", if spell_item.is_checked() { "true" } else { "false" })
                } else if event.id == complete_id {
                    ("complete", if complete_item.is_checked() { "true" } else { "false" })
                } else {
                    ("spell_dist", if conservative_item.is_checked() { "1" } else { "3" })
                };
                if let Err(error) = crate::settings::set_live(&control, key, value) {
                    crate::notify::notify("Settings unchanged", &error);
                }
                let config = crate::config::Config::global();
                spell_item.set_checked(config.spell_enabled);
                complete_item.set_checked(config.complete_enabled);
                conservative_item.set_checked(config.spell_max_dist == 1);
            } else if undo_items.iter().any(|(item, _)| *item.id() == event.id) {
                let value = undo_items.iter().find(|(item, _)| *item.id() == event.id).unwrap().1;
                if let Err(error) = crate::settings::set_live(&control, "undo_shortcut", value) {
                    crate::notify::notify("Shortcut unchanged", &error);
                }
                for (item, value) in &undo_items { item.set_checked(crate::config::Config::global().undo_shortcut == *value); }
            } else if mode_items.iter().any(|(item, _)| *item.id() == event.id) || excluded_items.iter().any(|(item, _)| *item.id() == event.id) {
                let selected = mode_items.iter().find(|(item, _)| *item.id() == event.id);
                let id = if selected.is_some() {
                    last_app.as_ref().map(|(_, id)| id.clone())
                } else {
                    excluded_items.iter().find(|(item, _)| *item.id() == event.id).map(|(_, id)| id.clone())
                };
                if let Some(id) = id {
                    let mode = selected.map(|(_, mode)| *mode).unwrap_or(crate::config::AppMode::Full);
                    if let Err(error) = crate::settings::set_app_mode(&control, &id, mode) {
                        crate::notify::notify("Application mode unchanged", &error);
                    }
                }
            } else if event.id == settings_id || event.id == ignored_id {
                let name = if event.id == settings_id { "config.toml" } else { "ignore.txt" };
                if let Err(error) = open_user_file(name) {
                    crate::notify::notify("Could not open ReCast file", &error.to_string());
                }
            } else if event.id == reload_id {
                // The watcher picks edits up on its own within a couple of
                // seconds; this is for the user who has just saved the file
                // and wants to know *now* that it took.
                crate::complete::reload_user_files();
                // `learned.txt` is ours rather than the user's, and is not
                // among the files a reload re-reads — so it is not reported by
                // the notification about having re-read them.
                let (abbrevs, ignored, _) = crate::complete::list_counts();
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
                #[cfg(target_os = "windows")]
                if let Some(tray) = &_tray {
                    windows_balloon(tray, None);
                }
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

/// Native balloon on the tray's hidden window: no foreground window or focus change.
#[cfg(target_os = "windows")]
fn windows_balloon(tray: &TrayIcon, message: Option<(&str, &str)>) {
    use winapi::um::{
        shellapi::*,
        winuser::{LoadIconW, IDI_INFORMATION},
    };
    unsafe {
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = tray.window_handle() as _;
        // A separate, temporary notification icon avoids depending on tray-icon's private ID.
        data.uID = u32::MAX;
        if let Some((title, body)) = message {
            data.uFlags = NIF_INFO | NIF_ICON | NIF_TIP;
            data.hIcon = LoadIconW(std::ptr::null_mut(), IDI_INFORMATION);
            data.dwInfoFlags = NIIF_INFO | NIIF_NOSOUND;
            for (dst, src) in data
                .szInfoTitle
                .iter_mut()
                .take(63)
                .zip(title.encode_utf16())
            {
                *dst = src;
            }
            for (dst, src) in data.szInfo.iter_mut().take(255).zip(body.encode_utf16()) {
                *dst = src;
            }
            for (dst, src) in data.szTip.iter_mut().zip("ReCast".encode_utf16()) {
                *dst = src;
            }
            Shell_NotifyIconW(NIM_ADD, &mut data);
        } else {
            Shell_NotifyIconW(NIM_DELETE, &mut data);
        }
    }
}

fn toggle_label(enabled: bool) -> &'static str {
    if enabled {
        "Disable"
    } else {
        "Enable"
    }
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
    if control.tighten_hint().is_some() && crate::config::Config::global().spell_max_dist > 1 {
        label.push_str(" — click to use Conservative spelling");
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
        if !row.is_empty() {
            row.push(' ');
        }
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

/// Create a missing file without truncating an existing user's settings.
fn prepare_file(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => file.write_all(contents.as_bytes()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

fn open_user_file(name: &str) -> std::io::Result<()> {
    let path = crate::complete::user_path(name).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "No config directory available",
        )
    })?;
    let contents = if name == "config.toml" {
        crate::settings::sample()
    } else {
        "# One word per line. Changes are picked up automatically.\n".to_string()
    };
    prepare_file(&path, &contents)?;
    open_text_file(&path)
}

#[cfg(target_os = "macos")]
fn open_text_file(path: &std::path::Path) -> std::io::Result<()> {
    let status = std::process::Command::new("open")
        .arg("-t")
        .arg(path)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "Text editor exited with {status}"
        )))
    }
}

#[cfg(target_os = "windows")]
fn open_text_file(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use winapi::um::{shellapi::ShellExecuteW, winuser::SW_SHOWNORMAL};
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if result > 32 {
        return Ok(());
    }
    // A fresh Windows install may have no association for .toml.
    if result == 31 {
        let mut child = std::process::Command::new("notepad.exe")
            .arg(path)
            .spawn()?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        return Ok(());
    }
    Err(std::io::Error::other(format!(
        "Windows could not open {} (code {result})",
        path.display()
    )))
}

#[cfg(test)]
mod file_tests {
    #[test]
    fn opening_settings_creates_once_and_preserves_edits() {
        let dir = std::env::temp_dir().join(format!("recast-editor-{}", std::process::id()));
        let path = dir.join("config.toml");
        super::prepare_file(&path, "spell = false\n").unwrap();
        super::prepare_file(&path, "spell = true\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "spell = false\n");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
