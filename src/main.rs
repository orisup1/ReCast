// Release builds on Windows run as a GUI app (no console window) so launching
// from Explorer behaves like a normal menubar/tray app — parity with macOS,
// which already lives only in the menubar. Debug builds keep the console so
// `println!` diagnostics remain visible during development.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod dictionary;
#[cfg(target_os = "linux")]
mod gui;
mod keymap;
mod layout;
mod platform;
mod types;

use std::sync::Arc;

use dictionary::parse_dictionary;
use types::AppControl;

// Dictionaries are baked into the binary at compile time so the executable is
// self-contained — it loads identically no matter what the current working
// directory is when launched.
const EN_DICT_TXT: &str = include_str!("../en_dict.txt");
const HE_DICT_TXT: &str = include_str!("../he_dict.txt");

fn main() {
    let with_gui = std::env::args().skip(1).any(|a| a == "-g" || a == "--gui");

    let en_dict = parse_dictionary(EN_DICT_TXT);
    let he_dict = parse_dictionary(HE_DICT_TXT);

    let control = Arc::new(AppControl::new());

    #[cfg(target_os = "linux")]
    {
        if with_gui {
            // Keyboard listener runs in a background thread; the GUI must own
            // the main thread because eframe drives the windowing event loop.
            let listener_control = Arc::clone(&control);
            std::thread::spawn(move || {
                platform::linux::run(en_dict, he_dict, listener_control);
            });
            if let Err(e) = gui::run(control) {
                eprintln!("GUI error: {}", e);
            }
            return;
        }
        platform::linux::run(en_dict, he_dict, control);
    }

    #[cfg(target_os = "macos")]
    {
        let _ = with_gui; // GUI flag is currently Linux-only.
        // Attach the CGEventTap to the main run loop *before* tao takes it
        // over. The tap callback fires from inside NSApp's event loop, so no
        // background thread is involved — running it on a side thread (the
        // earlier design) made macOS terminate the process after ~2s.
        // `_tap` must outlive `tray::run`; dropping it releases the tap.
        let _tap = platform::macos::setup_event_tap(en_dict, he_dict, Arc::clone(&control));
        // Menubar event loop must own the main thread (NSApp requirement).
        platform::tray::run(control);
    }

    #[cfg(target_os = "windows")]
    {
        let _ = with_gui;
        let listener_control = Arc::clone(&control);
        std::thread::spawn(move || {
            platform::windows::run(en_dict, he_dict, listener_control);
        });
        // Tray event loop must own the main thread (Win32 message pump).
        platform::tray::run(control);
    }
}
