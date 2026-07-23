use std::collections::HashSet;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rdev::{listen, simulate, Event, EventType, Key};

use crate::dictionary::check_and_switch_with_split;
use crate::keymap::{key_to_english_char, key_to_hebrew_char};
use crate::types::AppControl;

/// Maximum time the replace thread will wait for the user to physically
/// release the keys we are about to retype before injecting anyway.
const HELD_RELEASE_TIMEOUT: Duration = Duration::from_millis(150);

/// Gap between a synthetic key-down and its key-up, and between consecutive
/// keys. `SendInput` (used by rdev) is synchronous and reliable, so this can
/// be tighter than the macOS pacing, but the previous 50µs spacing was tight
/// enough that a backspace could be dropped in slow apps (leaving the original
/// first letter behind). 1ms gives a safe margin while staying fast.
const KEY_PRESS_GAP: Duration = Duration::from_millis(1);
const INTER_KEY_GAP: Duration = Duration::from_millis(1);

pub struct AppState {
    pub keys: Vec<Key>,
    pub is_replacing: bool,
    pub buffered_keys: Vec<Key>,
    /// Physical keys currently held down. Tracked from press/release events
    /// so the replace thread can wait for the user to lift the keys it is
    /// about to retype — otherwise the OS sees the synthetic press as a
    /// duplicate of the still-held physical key and drops it.
    pub held_keys: HashSet<Key>,
}

/// Reattach the process to the console of whatever launched it so the startup
/// ASCII-art banner can print.
///
/// Release builds set `windows_subsystem = "windows"` (see the crate attribute
/// in `main.rs`) so launching from Explorer never flashes a console window.
/// The side effect is that the process starts with *no* console and null
/// standard handles even when the user ran `recast` from an existing terminal —
/// so `IsTerminal(stdout)` is false and `banner::print_logo` prints nothing.
///
/// `AttachConsole(ATTACH_PARENT_PROCESS)` borrows the launching shell's console;
/// we then repoint the standard handles at its `CONOUT$`/`CONIN$` buffers (a
/// GUI-subsystem process's handles aren't wired up automatically) and enable
/// virtual-terminal processing so the banner's ANSI color escapes render.
///
/// When there is no parent console — Explorer double-click, the logon Scheduled
/// Task — `AttachConsole` fails and this is a silent no-op, exactly matching the
/// old behaviour of no banner. In debug builds the process already owns a
/// console, so `AttachConsole` also fails harmlessly and stdout is untouched.
///
/// Must run before the first stdout access (i.e. before `banner::print_logo`).
pub fn attach_parent_console() {
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    use winapi::um::consoleapi::{GetConsoleMode, SetConsoleMode};
    use winapi::um::fileapi::{CreateFileW, OPEN_EXISTING};
    use winapi::um::handleapi::INVALID_HANDLE_VALUE;
    use winapi::um::processenv::SetStdHandle;
    use winapi::um::winbase::{STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    use winapi::um::wincon::{
        AttachConsole, ATTACH_PARENT_PROCESS, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    };
    use winapi::um::winnt::{FILE_SHARE_READ, FILE_SHARE_WRITE, GENERIC_READ, GENERIC_WRITE};

    let wide = |s: &str| -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain(once(0)).collect()
    };

    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            // No parent console to attach to (Explorer / Scheduled Task launch),
            // or one is already attached (debug build): leave stdio as-is.
            return;
        }

        // Open the attached console's screen buffer and repoint stdout/stderr at
        // it; Rust's stdio reads the std handle on each write, so setting it here
        // (before any output) is enough for `println!` and `is_terminal()`.
        let conout = CreateFileW(
            wide("CONOUT$").as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null_mut(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        );
        if conout != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_OUTPUT_HANDLE, conout);
            SetStdHandle(STD_ERROR_HANDLE, conout);
            // Turn on ANSI escape interpretation so the banner's colors show as
            // colors rather than raw `\x1b[` gibberish.
            let mut mode = 0u32;
            if GetConsoleMode(conout, &mut mode) != 0 {
                SetConsoleMode(conout, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
            }
        }

        let conin = CreateFileW(
            wide("CONIN$").as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null_mut(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        );
        if conin != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_INPUT_HANDLE, conin);
        }
    }
}

/// Full Windows startup. Owns everything that used to live in `main`'s Windows
/// `cfg` block: run the keyboard listener on a background thread and hand the
/// main thread to the tray (or the TUI with `--gui`). Keeping it here means
/// changes to the Windows launch path can't touch the Linux or macOS paths.
pub fn start(
    en: &'static HashSet<String>,
    he: &'static HashSet<String>,
    control: Arc<AppControl>,
    with_gui: bool,
) {
    if with_gui {
        let listener_control = Arc::clone(&control);
        thread::spawn(move || {
            run(en, he, listener_control);
        });
        if let Err(e) = crate::tui::run_tui(control) {
            let _ = writeln!(std::io::stderr(), "TUI error: {e}");
        }
        return;
    }
    // No daemonization on Windows: listener thread plus tray on the main thread.
    let listener_control = Arc::clone(&control);
    thread::spawn(move || {
        run(en, he, listener_control);
    });
    crate::platform::tray::run(control);
}

pub fn run(en_dict: &'static HashSet<String>, he_dict: &'static HashSet<String>, control: Arc<AppControl>) {
    // Non-panicking logging: a release build sets `windows_subsystem = "windows"`,
    // so there is no console and a plain println!/eprintln! returns Err and
    // PANICS. Because this function runs on the listener thread, that panic would
    // silently kill keyboard capture while the tray kept running — the app would
    // look alive but correct nothing. Ignore write errors instead.
    let _ = writeln!(std::io::stdout(), "Starting recast keyboard watcher (Windows).");


    let control_cb = Arc::clone(&control);
    let state: Arc<Mutex<AppState>> = Arc::new(Mutex::new(AppState {
        keys: Vec::new(),
        is_replacing: false,
        buffered_keys: Vec::new(),
        held_keys: HashSet::new(),
    }));
    let state_cb = Arc::clone(&state);
    let injecting = Arc::new(AtomicBool::new(false));
    let injecting_cb = Arc::clone(&injecting);

    let callback = move |event: Event| {
        // Ignore the key events we generate ourselves; otherwise the listener
        // treats them as user input and interferes with the replacement.
        if injecting_cb.load(Ordering::Relaxed) {
            return;
        }

        let mut st = state_cb.lock().unwrap();
        match event.event_type {
            EventType::KeyPress(key) => {
                st.held_keys.insert(key);
                match key {
                    Key::Space | Key::Return => {
                        if st.is_replacing {
                            st.buffered_keys.push(key);
                            return;
                        }

                        if !st.keys.is_empty() {
                            if !control_cb.is_enabled() {
                                st.keys.clear();
                                return;
                            }
                            let result = check_and_switch_with_split(
                                &st.keys,
                                key_to_english_char,
                                key_to_hebrew_char,
                                en_dict,
                                he_dict,
                            );

                            if let Some(start) = result {
                                control_cb.record_fix();
                                st.is_replacing = true;
                                // See linux.rs: anything before `start` is a
                                // previously-typed word the user concatenated
                                // by forgetting a space; leave it untouched.
                                let keys_clone: Vec<Key> = st.keys[start..].to_vec();
                                let terminator = key;
                                let state_clone = Arc::clone(&state_cb);
                                let injecting_flag = Arc::clone(&injecting);

                                thread::spawn(move || {
                                    replace_word(
                                        keys_clone,
                                        terminator,
                                        &state_clone,
                                        &injecting_flag,
                                    );
                                });
                            }

                            st.keys.clear();
                        }
                    }
                    Key::Backspace => {
                        if st.is_replacing {
                            st.buffered_keys.pop();
                        } else {
                            st.keys.pop();
                        }
                    }
                    Key::Tab
                    | Key::Escape
                    | Key::LeftArrow
                    | Key::RightArrow
                    | Key::UpArrow
                    | Key::DownArrow
                    | Key::Home
                    | Key::End
                    | Key::PageUp
                    | Key::PageDown
                    | Key::Insert
                    | Key::Delete => {
                        if st.is_replacing {
                            st.buffered_keys.clear();
                        } else {
                            st.keys.clear();
                        }
                    }
                    _ => {
                        if key_to_english_char(key).is_some()
                            || key_to_hebrew_char(key).is_some()
                        {
                            if st.is_replacing {
                                st.buffered_keys.push(key);
                            } else {
                                st.keys.push(key);
                            }
                        }
                    }
                }
            }
            EventType::KeyRelease(key) => {
                st.held_keys.remove(&key);
            }
            EventType::ButtonPress(_) => {
                if st.is_replacing {
                    st.buffered_keys.clear();
                } else {
                    st.keys.clear();
                }
            }
            _ => {}
        }
    };

    let _ = writeln!(std::io::stdout(), "Listening for keyboard events.");
    if let Err(err) = listen(callback) {
        let _ = writeln!(std::io::stderr(), "Error while listening for keyboard events: {err:?}");
    }
}

/// After a layout switch, erase the mistyped word and retype it in the new layout.
fn replace_word(
    keys: Vec<Key>,
    terminator: Key,
    state_mutex: &Arc<Mutex<AppState>>,
    injecting: &Arc<AtomicBool>,
) {
    // 1. Wait for the user to physically release the terminator and any of
    //    the word's keys before injecting. injecting=false here so release
    //    events from the listener still update held_keys.
    let mut keys_of_interest: HashSet<Key> = keys.iter().copied().collect();
    keys_of_interest.insert(terminator);
    let wait_start = Instant::now();
    loop {
        let still_held = {
            let st = state_mutex.lock().unwrap();
            keys_of_interest.iter().any(|k| st.held_keys.contains(k))
        };
        if !still_held {
            break;
        }
        if wait_start.elapsed() >= HELD_RELEASE_TIMEOUT {
            break;
        }
        thread::sleep(Duration::from_micros(100));
    }

    // 2. switch_layout_to already polled until the layout change took effect,
    //    so no settle delay is needed here.

    // 3. Gate the listener now that we are about to inject our own events.
    injecting.store(true, Ordering::Relaxed);

    let buf = {
        let st = state_mutex.lock().unwrap();
        st.buffered_keys.clone()
    };

    // Press + release a single key with pacing the OS won't drop.
    let tap_key = |k: Key| {
        let _ = simulate(&EventType::KeyPress(k));
        thread::sleep(KEY_PRESS_GAP);
        let _ = simulate(&EventType::KeyRelease(k));
        thread::sleep(INTER_KEY_GAP);
    };

    // +1 for the terminator the user physically typed.
    let delete_count = keys.len() + 1 + buf.len();
    for _ in 0..delete_count {
        tap_key(Key::Backspace);
    }
    for k in &keys {
        tap_key(*k);
    }
    tap_key(terminator);
    for k in buf.iter() {
        tap_key(*k);
    }

    let mut st = state_mutex.lock().unwrap();
    st.keys = buf;
    st.buffered_keys.clear();
    st.is_replacing = false;
    injecting.store(false, Ordering::Relaxed);
}
