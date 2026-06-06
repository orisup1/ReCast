use std::process;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::types::AppControl;

/// How often the menu's "Fixed: N" counter is refreshed while idle.
const STATUS_REFRESH: Duration = Duration::from_millis(750);

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
    // Informational row showing fixed-word count.
    let status_item = MenuItem::new(status_label(control.fixed_count()), false, None);
    let toggle_item = MenuItem::new(toggle_label(control.is_enabled()), true, None);
    let sep = MenuItem::new("", false, None);
    let about_item = MenuItem::new("About ReCast", true, None);
    let quit_item = MenuItem::new("Quit", true, None);
    menu.append(&status_item).expect("append status");
    menu.append(&toggle_item).expect("append toggle");
    menu.append(&sep).expect("append separator");
    menu.append(&about_item).expect("append about");
    menu.append(&quit_item).expect("append quit");

    let toggle_id = toggle_item.id().clone();
    let about_id = about_item.id().clone();
    let quit_id = quit_item.id().clone();
    let menu_channel = MenuEvent::receiver();

    // Track the last rendered count so we only rewrite the label when it
    // changes, avoiding needless native menu churn on every timer wake.
    let mut last_count = control.fixed_count();

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

        // Keep the counter in sync with the listener's running total.
        let count = control.fixed_count();
        if count != last_count {
            last_count = count;
            status_item.set_text(status_label(count));
        }

        if let Event::NewEvents(StartCause::Init) = event {
            if let Some(menu) = pending_menu.take() {
                let icon = app_icon();
                #[allow(unused_mut)]
                let mut tray_builder = TrayIconBuilder::new()
                    .with_menu(Box::new(menu))
                    .with_tooltip({
                        let enabled = control.is_enabled();
                        let count = control.fixed_count();
                        format!("ReCast - {} - {} fixed", if enabled { "Enabled" } else { "Disabled" }, count)
                    })
                    .with_icon(icon);
                #[cfg(target_os = "macos")]
                {
                    tray_builder = tray_builder.with_title("ReCast");
                }
                _tray = Some(tray_builder.build().expect("tray build"));
            }
        }

        while let Ok(event) = menu_channel.try_recv() {
            if event.id == toggle_id {
                let new_enabled = !control.is_enabled();
                control.set_enabled(new_enabled);
                toggle_item.set_text(toggle_label(new_enabled));
                // Update tooltip immediately
                let _ = _tray.as_ref().map(|t| t.set_tooltip({
                    let count = control.fixed_count();
                    format!("ReCast - {} - {} fixed", if new_enabled { "Enabled" } else { "Disabled" }, count)
                }));
            } else if event.id == about_id {
                // Show about dialog - platform specific
                #[cfg(target_os = "macos")]
                {
                    use cocoa::appkit::{NSApp, NSAlert};
                    use cocoa::base::nil;
                    unsafe {
                        let alert = NSAlert::alloc(nil);
                        alert.setMessageText("ReCast");
                        alert.setInformativeText("Layout mistake fixer for bilingual typing.\n\n© 2026");
                        alert.addButtonWithTitle("OK");
                        alert.runModal();
                    }
                }
                #[cfg(target_os = "windows")]
                {
                    use winapi::um::winuser::{MessageBoxW, MB_OK};
                    use winapi::shared::windef::HWND;
                    use std::ffi::OsString;
                    use std::os::windows::ffi::OsStringExt;
                    let text = OsString::from("ReCast\nLayout mistake fixer for bilingual typing.\n\n© 2026");
                    let wide: Vec<u16> = text.encode_wide().chain(std::iter::once(0)).collect();
                    let caption = OsString::from("ReCast").encode_wide().chain(std::iter::once(0)).collect::<Vec<_>>();
                    unsafe {
                        MessageBoxW(std::ptr::null_mut(), wide.as_ptr(), caption.as_ptr(), MB_OK);
                    }
                }
            } else if event.id == quit_id {
                process::exit(0);
            }
        }
    });
}

fn toggle_label(enabled: bool) -> &'static str {
    if enabled { "Disable" } else { "Enable" }
}

fn status_label(fixed: u64) -> String {
    format!("Fixed: {}", fixed)
}

/// The ReCast keycap/swap-arrows icon, baked in as raw 32×32 RGBA at compile
/// time (generated from `assets/recast-icon.svg` → `assets/tray-icon.rgba`).
/// Self-contained, so the binary needs no icon file at runtime.
const ICON_RGBA: &[u8] = include_bytes!("../../assets/tray-icon.rgba");
const ICON_SIZE: u32 = 32;

fn app_icon() -> Icon {
    Icon::from_rgba(ICON_RGBA.to_vec(), ICON_SIZE, ICON_SIZE).expect("icon build")
}
