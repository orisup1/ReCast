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
    // Disabled informational row mirroring the Linux GUI's fixed-word counter.
    let status_item = MenuItem::new(status_label(control.fixed_count()), false, None);
    let toggle_item = MenuItem::new(toggle_label(control.is_enabled()), true, None);
    let quit_item = MenuItem::new("Quit", true, None);
    menu.append(&status_item).expect("append status");
    menu.append(&toggle_item).expect("append toggle");
    menu.append(&quit_item).expect("append quit");

    let toggle_id = toggle_item.id().clone();
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
                let icon = placeholder_icon();
                #[allow(unused_mut)]
                let mut tray_builder = TrayIconBuilder::new()
                    .with_menu(Box::new(menu))
                    .with_tooltip("ReCast")
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

fn placeholder_icon() -> Icon {
    let size: u32 = 16;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for _ in 0..(size * size) {
        rgba.extend_from_slice(&[20, 120, 200, 255]);
    }
    Icon::from_rgba(rgba, size, size).expect("icon build")
}
