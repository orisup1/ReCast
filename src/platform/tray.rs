use std::process;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{TrayIcon, TrayIconBuilder};

use crate::types::AppControl;

/// How often the menu's counters are refreshed while idle.
const STATUS_REFRESH: Duration = Duration::from_millis(750);

/// How long "Pause" pauses for. Long enough to get through the thing that
/// prompted it (a terminal session, a password-heavy form, dictating a name),
/// short enough that forgetting to switch it back on isn't a silent week
/// without corrections — which is the failure mode of a plain Disable.
const PAUSE_LENGTH: Duration = Duration::from_secs(30 * 60);

/// Run the native menu and its modeless windows on the main thread.
pub fn run(control: Arc<AppControl>) {
    let mut builder = EventLoopBuilder::new();
    #[allow(unused_mut)]
    let mut event_loop = builder.build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }
    let proxy = event_loop.create_proxy();
    let menu = Menu::new();
    let health_menu = Submenu::new("Starting keyboard listener…", true);
    let health_detail = MenuItem::new("Starting keyboard listener…", false, None);
    let status_item = MenuItem::new(status_label(&control), false, None);
    let conservative_item = MenuItem::new("Use conservative spelling", false, None);
    health_menu
        .append(&health_detail)
        .expect("append health details");
    health_menu.append(&status_item).expect("append counters");
    health_menu
        .append(&conservative_item)
        .expect("append sensitivity hint");

    let recovery_item = MenuItem::new("Open Settings to resolve…", true, None);
    health_menu.append(&recovery_item).expect("append recovery");
    let app_menu = Submenu::new("Application mode (waiting for focus)", false);
    let app_modes = crate::config::AppMode::ALL.map(|mode| {
        let item = CheckMenuItem::new(mode.label(), true, false, None);
        app_menu.append(&item).expect("append app mode");
        (mode, item)
    });
    let pause_menu = Submenu::new("Pause correction", true);
    let pause_item = MenuItem::new(pause_label(None), true, None);
    let app_pause_item = MenuItem::new("Pause in application (waiting for focus)", false, None);
    let toggle_item = MenuItem::new(toggle_label(control.is_switched_on()), true, None);
    pause_menu.append(&pause_item).expect("append timed pause");

    pause_menu
        .append(&PredefinedMenuItem::separator())
        .expect("append separator");
    pause_menu
        .append(&toggle_item)
        .expect("append enable control");

    let recent_item = MenuItem::new("Recent corrections…", true, None);
    let settings_item = MenuItem::new("Settings…", true, None);
    let preference_menu = Submenu::new("Language preference", true);
    let preference_items = crate::config::LanguagePreference::ALL.map(|preference| {
        let item = CheckMenuItem::new(
            preference.label(),
            true,
            crate::config::Config::global().language_preference == preference,
            None,
        );
        preference_menu
            .append(&item)
            .expect("append language preference");
        (preference, item)
    });
    let help_menu = Submenu::new("Help", true);
    let shortcuts_item = MenuItem::new("Typing shortcuts…", true, None);
    let practice_item = MenuItem::new("Practice correction and undo…", true, None);
    let start_log_item = MenuItem::new("Start live log…", true, None);
    let stop_log_item = MenuItem::new("Stop live log", false, None);
    let about_item = MenuItem::new("About ReCast", true, None);
    for item in [
        &shortcuts_item,
        &practice_item,
        &start_log_item,
        &stop_log_item,
        &about_item,
    ] {
        help_menu.append(item).expect("append help action");
    }
    let quit_item = MenuItem::new("Quit ReCast", true, None);
    menu.append(&health_menu).expect("append status");
    menu.append(&app_menu).expect("append current app");
    menu.append(&app_pause_item).expect("append quick pause");
    menu.append(&pause_menu).expect("append pause");
    menu.append(&PredefinedMenuItem::separator())
        .expect("append separator");
    menu.append(&recent_item).expect("append history");
    menu.append(&preference_menu)
        .expect("append language preference");
    menu.append(&settings_item).expect("append settings");
    menu.append(&help_menu).expect("append help");
    menu.append(&PredefinedMenuItem::separator())
        .expect("append separator");
    menu.append(&quit_item).expect("append quit");
    let menu_channel = MenuEvent::receiver();
    let mut pending_menu = Some(menu);
    let mut tray: Option<TrayIcon> = None;
    let mut last_icon = None;
    let mut health = String::new();
    let mut last_health_check = Instant::now() - STATUS_REFRESH;
    let mut last_app = None;
    let mut practice_window: Option<crate::practice::native::Window> = None;
    let mut preferences_window: Option<super::preferences::Window> = None;
    let mut history_window: Option<super::history::Window> = None;
    let mut log_window: Option<super::log_viewer::Window> = None;
    let mut practice_offered = false;
    let mut last_ui_error = String::new();
    #[cfg(target_os = "windows")]
    let mut balloon_until: Option<Instant> = None;

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + STATUS_REFRESH);
        if let Some(app) = super::active_application() {
            last_app = Some(app);
        }

        if let Event::WindowEvent {
            window_id,
            event: WindowEvent::CloseRequested,
            ..
        } = &event
        {
            if practice_window
                .as_ref()
                .is_some_and(|w| w.form.window.id() == *window_id)
            {
                control
                    .practice_open
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                practice_window = None;
            }
            if preferences_window
                .as_ref()
                .is_some_and(|w| w.form.window.id() == *window_id)
            {
                preferences_window = None;
            }
            if history_window
                .as_ref()
                .is_some_and(|w| w.form.window.id() == *window_id)
            {
                history_window = None;
            }
            if log_window
                .as_ref()
                .is_some_and(|w| w.form.window.id() == *window_id)
            {
                control.live_log.close();
                log_window = None;
                start_log_item.set_text("Start live log…");
            }
        }
        if last_health_check.elapsed() >= STATUS_REFRESH {
            health = super::status(&control);
            observe_log_health(&control, &health);
            let (state, detail) = health.split_once(" — ").unwrap_or((&health, ""));
            health_menu.set_text(state);
            health_detail.set_text(if detail.is_empty() {
                "Correction status"
            } else {
                detail
            });
            status_item.set_text(status_label(&control));
            if let Some((name, id)) = &last_app {
                let mode = control.app_mode(Some(id));
                app_menu.set_text(format!("{name}: {}", mode.map_or("Unknown", |m| m.label())));
                app_menu.set_enabled(true);
                for (value, item) in &app_modes {
                    item.set_checked(mode == Some(*value));
                }
            }
            recovery_item.set_text(if !control.is_switched_on() {
                "Enable correction"
            } else if control.pause_remaining().is_some() || control.paused_app().is_some() {
                "Resume correction"
            } else if super::tray_icon::state(&health) == super::tray_icon::State::Attention {
                if cfg!(target_os = "macos") {
                    "Open system settings…"
                } else {
                    "Keyboard setup help…"
                }
            } else {
                "Settings / application modes…"
            });
            let preference = crate::config::Config::global().language_preference;
            for (value, item) in &preference_items {
                item.set_checked(*value == preference);
            }
            conservative_item.set_enabled(
                control.tighten_hint().is_some()
                    && crate::config::Config::global().spell_max_dist > 1,
            );
            pause_menu.set_text(
                if control.pause_remaining().is_some() || control.paused_app().is_some() {
                    "Resume / pause correction"
                } else {
                    "Pause correction"
                },
            );
            pause_item.set_text(pause_label(control.pause_remaining()));
            toggle_item.set_text(toggle_label(control.is_switched_on()));
            pause_item.set_enabled(control.is_switched_on());
            if let Some(id) = control.paused_app() {
                app_pause_item.set_text(format!("Resume in {id}"));
                app_pause_item.set_enabled(true);
            } else if let Some((name, _)) = &last_app {
                app_pause_item.set_text(format!("Pause in {name} until I switch away"));
                app_pause_item.set_enabled(control.is_switched_on());
            }
            if let Some(tray) = &tray {
                let _ = tray.set_tooltip(Some(format!("ReCast — {health}")));
                let icon = super::tray_icon::state(&health);
                if last_icon != Some(icon) {
                    #[cfg(target_os = "macos")]
                    let _ =
                        tray.set_icon_with_as_template(Some(super::tray_icon::icon(icon)), true);
                    #[cfg(target_os = "windows")]
                    let _ = tray.set_icon(Some(super::tray_icon::icon(icon)));
                    last_icon = Some(icon);
                }
            }
            last_health_check = Instant::now();
        }
        if let Event::NewEvents(StartCause::Init) = event {
            if let Some(menu) = pending_menu.take() {
                let state = super::tray_icon::state(&health);
                #[allow(unused_mut)]
                let mut builder = TrayIconBuilder::new()
                    .with_menu(Box::new(menu))
                    .with_tooltip(format!("ReCast — {health}"))
                    .with_icon(super::tray_icon::icon(state));
                #[cfg(target_os = "macos")]
                {
                    builder = builder.with_icon_as_template(true);
                }
                tray = Some(builder.build().expect("tray build"));
                last_icon = Some(state);
            }
        }

        if !practice_offered
            && pending_menu.is_none()
            && control
                .listener_ready
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            practice_offered = true;
            let (english, hebrew) = crate::layout::enabled_languages();
            if crate::practice::first_run() || !english || !hebrew {
                match crate::practice::native::Window::new(target, proxy.clone(), &control) {
                    Ok(window) => practice_window = Some(window),
                    Err(error) => {
                        control
                            .practice_open
                            .store(false, std::sync::atomic::Ordering::Relaxed);
                        crate::notify::notify("Could not open practice", &error);
                    }
                }
            }
        }
        if let Some(practice) = &mut practice_window {
            match practice.update(&control, &health) {
                Ok(true) => {
                    control
                        .practice_open
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    practice_window = None;
                }
                Ok(false) => (),
                Err(error) => report_ui_error(&mut last_ui_error, error),
            }
        }
        if let Some(preferences) = &mut preferences_window {
            match preferences.update(&control, last_app.as_ref()) {
                Ok(actions) => {
                    for action in actions {
                        match action {
                            super::preferences::Action::OpenConfig => {
                                if let Err(error) = open_user_file("config.toml") {
                                    crate::notify::notify(
                                        "Could not open settings",
                                        &error.to_string(),
                                    );
                                }
                            }
                            super::preferences::Action::Edit(kind) => {
                                super::word_rules::edit(kind, &control)
                            }
                        }
                    }
                }
                Err(error) => report_ui_error(&mut last_ui_error, error),
            }
        }
        if let Some(history) = &mut history_window {
            if let Err(error) = history.update(&control) {
                report_ui_error(&mut last_ui_error, error);
            }
        }
        if let Some(log) = &mut log_window {
            if let Err(error) = log.update(&control.live_log) {
                report_ui_error(&mut last_ui_error, error);
            }
        }
        stop_log_item.set_enabled(control.live_log.active());

        #[cfg(target_os = "windows")]
        if let Some(tray) = &tray {
            if balloon_until.is_some_and(|until| Instant::now() >= until) {
                windows_balloon(tray, None);
                balloon_until = None;
            }
            if balloon_until.is_none() {
                let notice = crate::notify::WINDOWS_NOTICES
                    .lock()
                    .ok()
                    .and_then(|mut q| q.pop_front());
                if let Some((title, body)) = notice {
                    windows_balloon(tray, Some((&title, &body)));
                    balloon_until = Some(Instant::now() + Duration::from_secs(12));
                }
            }
        }

        while let Ok(event) = menu_channel.try_recv() {
            if let Some((preference, _)) = preference_items
                .iter()
                .find(|(_, item)| event.id == *item.id())
            {
                if let Err(error) =
                    crate::settings::set_live(&control, "prefer", preference.value())
                {
                    crate::notify::notify("Settings unchanged", &error);
                }
                let selected = crate::config::Config::global().language_preference;
                for (value, item) in &preference_items {
                    item.set_checked(*value == selected);
                }
            } else if let Some((mode, _)) =
                app_modes.iter().find(|(_, item)| event.id == *item.id())
            {
                if let Some((_, id)) = &last_app {
                    if let Err(error) = crate::settings::set_app_mode(&control, id, *mode) {
                        crate::notify::notify("Application mode unchanged", &error);
                    }
                }
            } else if event.id == *conservative_item.id() {
                if let Err(error) = crate::settings::set_live(&control, "spell_dist", "1") {
                    crate::notify::notify("Settings unchanged", &error);
                }
            } else if event.id == *toggle_item.id() {
                control.set_enabled(!control.is_switched_on());
            } else if event.id == *app_pause_item.id() {
                if control.paused_app().is_some() {
                    control.resume_app();
                } else if let Some((_, id)) = &last_app {
                    control.pause_in_app(id);
                }
            } else if event.id == *pause_item.id() {
                if control.pause_remaining().is_some() {
                    control.resume();
                } else {
                    control.pause_for(PAUSE_LENGTH);
                }
            } else if event.id == *recovery_item.id() && !control.is_switched_on() {
                control.set_enabled(true);
            } else if event.id == *recovery_item.id()
                && (control.pause_remaining().is_some() || control.paused_app().is_some())
            {
                control.resume();
                control.resume_app();
            } else if event.id == *recovery_item.id()
                && super::tray_icon::state(&health) == super::tray_icon::State::Attention
            {
                #[cfg(target_os = "macos")]
                {
                    if let Err(error) = super::macos::open_setup_settings() {
                        crate::notify::notify("Could not open system settings", &error);
                    }
                }
                #[cfg(target_os = "windows")]
                {
                    match crate::practice::native::Window::new(target, proxy.clone(), &control) {
                        Ok(window) => practice_window = Some(window),
                        Err(error) => crate::notify::notify("Could not open setup help", &error),
                    }
                }
            } else if event.id == *settings_item.id() || event.id == *recovery_item.id() {
                if let Some(window) = &preferences_window {
                    window.form.show();
                } else {
                    match super::preferences::Window::new(
                        target,
                        proxy.clone(),
                        &control,
                        last_app.as_ref(),
                    ) {
                        Ok(window) => preferences_window = Some(window),
                        Err(error) => crate::notify::notify("Could not open Settings", &error),
                    }
                }
            } else if event.id == *recent_item.id() {
                if let Some(window) = &history_window {
                    window.form.show();
                } else {
                    match super::history::Window::new(target, proxy.clone(), &control) {
                        Ok(window) => history_window = Some(window),
                        Err(error) => crate::notify::notify("Could not open history", &error),
                    }
                }
            } else if event.id == *practice_item.id() {
                if let Some(practice) = &practice_window {
                    practice.form.show();
                    practice.focus();
                } else {
                    match crate::practice::native::Window::new(target, proxy.clone(), &control) {
                        Ok(window) => practice_window = Some(window),
                        Err(error) => {
                            control
                                .practice_open
                                .store(false, std::sync::atomic::Ordering::Relaxed);
                            crate::notify::notify("Could not open practice", &error);
                        }
                    }
                }
            } else if event.id == *start_log_item.id() {
                let initial_health = super::status(&control);
                if let Some(log) = &log_window {
                    if !control.live_log.active() {
                        control.live_log.start();
                        observe_log_health(&control, &initial_health);
                    }
                    log.focus();
                } else {
                    match super::log_viewer::Window::new(target, proxy.clone()) {
                        Ok(mut window) => {
                            control.live_log.start();
                            observe_log_health(&control, &initial_health);
                            if let Err(error) = window.update(&control.live_log) {
                                report_ui_error(&mut last_ui_error, error);
                            }
                            log_window = Some(window);
                        }
                        Err(error) => crate::notify::notify("Could not open live log", &error),
                    }
                }
                if log_window.is_some() {
                    start_log_item.set_text("Show live log…");
                }
            } else if event.id == *stop_log_item.id() {
                control.live_log.stop();
                start_log_item.set_text("Start live log…");
            } else if event.id == *shortcuts_item.id() {
                crate::notify::show_shortcuts();
            } else if event.id == *about_item.id() {
                show_about();
            } else if event.id == *quit_item.id() {
                #[cfg(target_os = "macos")]
                if let Err(error) = crate::daemon::stop_launch_agent(process::id()) {
                    crate::notify::notify("Could not quit ReCast", &error.to_string());
                    continue;
                }
                #[cfg(target_os = "windows")]
                if let Some(tray) = &tray {
                    windows_balloon(tray, None);
                }
                // Remove the notification-area icon before exiting without destructors.
                let _ = tray.take();
                process::exit(0);
            }
            last_health_check = Instant::now() - STATUS_REFRESH;
        }
    });
}

fn report_ui_error(previous: &mut String, error: String) {
    if *previous != error {
        crate::notify::notify("Could not update ReCast window", &error);
        *previous = error;
    }
}

fn show_about() {
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
        use objc::sel;
        use objc::sel_impl;
        use objc::{class, msg_send};

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
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStrExt;
        use winapi::um::winuser::{MessageBoxW, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND};
        let body = format!(
        "Layout mistake fixer for bilingual typing.\n\nVersion {}\n\nCreated by Ori Supino\n© 2026 Ori Supino",
        env!("CARGO_PKG_VERSION")
    );
        let wide: Vec<u16> = OsString::from(body)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let caption: Vec<u16> = OsString::from("About ReCast")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
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
}

fn observe_log_health(control: &AppControl, health: &str) {
    if !control.live_log.active() {
        return;
    }
    #[cfg(target_os = "macos")]
    if !super::macos::accessibility_granted() {
        control.live_log.observe_health("Accessibility permission unavailable — grant ReCast access in Privacy & Security → Accessibility, then relaunch.");
        return;
    }
    control.live_log.observe_health(health);
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
        "Disable until I enable it again"
    } else {
        "Enable correction"
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
        label.push_str(" · consider conservative spelling");
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
