use std::collections::HashSet;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rdev::{listen, Event, EventType, Key};
use winapi::ctypes::c_int;
use winapi::um::winuser::{
    SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VK_BACK,
    VK_RETURN, VK_SPACE,
};

use crate::dictionary::{check_and_correct, Dict, Fix};
use crate::keymap::{key_to_english_char, key_to_hebrew_char};
use crate::types::AppControl;

/// Maximum time the replace thread will wait for the user to physically
/// release the keys we are about to retype before injecting anyway.
const HELD_RELEASE_TIMEOUT: Duration = Duration::from_millis(150);

/// Grace period between the last injected event and un-gating the listener.
/// `SendInput` returns as soon as the events are queued, so clearing the gate
/// immediately can let our own tail end up back in the word buffer.
const INJECT_SETTLE: Duration = Duration::from_millis(2);

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
    en: Dict,
    he: Dict,
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

pub fn run(en_dict: Dict, he_dict: Dict, control: Arc<AppControl>) {
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
                            let result = check_and_correct(
                                &st.keys,
                                key_to_english_char,
                                key_to_hebrew_char,
                                en_dict,
                                he_dict,
                            );

                            if let Some((erase, text)) = replacement(&st.keys, result) {
                                control_cb.record_fix();
                                st.is_replacing = true;
                                let terminator = key;
                                let state_clone = Arc::clone(&state_cb);
                                let injecting_flag = Arc::clone(&injecting);

                                thread::spawn(move || {
                                    replace_word(
                                        erase,
                                        text,
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

/// Turn a [`Fix`] into what the replace thread needs: how many of the typed
/// keys have to be erased, and the finished text to put in their place.
///
/// Unlike Linux (which can only replay keycodes through `uinput`), Windows can
/// insert the characters themselves, so both kinds of fix reduce to the same
/// thing — erase N, insert a string — and neither depends on the keys being
/// typeable under the active layout.
fn replacement(keys: &[Key], fix: Option<Fix>) -> Option<(usize, String)> {
    match fix? {
        // Anything before `start` is a previously-typed word the user
        // concatenated by forgetting a space; leave it intact.
        Fix::Layout { start, text } => Some((keys.len() - start, text)),
        Fix::Spelling { text } => Some((keys.len(), text)),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Text injection via SendInput.
//
// `rdev::simulate` sends one key at a time and needs pacing between events, so
// a correction typed itself out over tens of milliseconds. `SendInput` takes
// the *whole* sequence — the backspaces, the corrected word as Unicode, and the
// terminator — in a single call, and the OS delivers it as one uninterrupted
// burst: the word is replaced the way a paste lands, not the way typing does.
//
// `KEYEVENTF_UNICODE` events carry the character rather than a key position, so
// the injected text is exactly what we computed regardless of which layout is
// active — the layout switch still happens (for what the user types next), but
// the correction no longer depends on it having propagated.
// ─────────────────────────────────────────────────────────────────────────────

/// One keyboard `INPUT`: a virtual-key press/release when `unicode` is `None`,
/// otherwise a UTF-16 code unit typed as text.
fn key_input(vk: u16, unicode: Option<u16>, up: bool) -> INPUT {
    let mut input: INPUT = unsafe { std::mem::zeroed() };
    input.type_ = INPUT_KEYBOARD;
    let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
    let (vk, scan) = match unicode {
        Some(unit) => {
            flags |= KEYEVENTF_UNICODE;
            (0, unit)
        }
        None => (vk, 0),
    };
    unsafe {
        *input.u.ki_mut() = KEYBDINPUT {
            wVk: vk,
            wScan: scan,
            dwFlags: flags,
            time: 0,
            dwExtraInfo: 0,
        };
    }
    input
}

fn press(vk: u16, out: &mut Vec<INPUT>) {
    out.push(key_input(vk, None, false));
    out.push(key_input(vk, None, true));
}

fn type_text(text: &str, out: &mut Vec<INPUT>) {
    for unit in text.encode_utf16() {
        out.push(key_input(0, Some(unit), false));
        out.push(key_input(0, Some(unit), true));
    }
}

/// Hand the whole batch to the OS in one call.
fn send(inputs: &mut [INPUT]) {
    if inputs.is_empty() {
        return;
    }
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_mut_ptr(),
            std::mem::size_of::<INPUT>() as c_int,
        );
    }
}

/// Erase the word the user typed and put the corrected one in its place, in a
/// single `SendInput` burst.
fn replace_word(
    erase: usize,
    text: String,
    terminator: Key,
    state_mutex: &Arc<Mutex<AppState>>,
    injecting: &Arc<AtomicBool>,
) {
    // 1. The corrected word goes in as text, so the only real key we press is
    //    Return (a space rides along inside the text instead). Wait for the
    //    user to lift it first: a synthetic press of a still-held key is
    //    swallowed as a duplicate. injecting=false here so release events from
    //    the listener still update held_keys.
    let needs_return = terminator == Key::Return;
    if needs_return {
        let wait_start = Instant::now();
        loop {
            let still_held = {
                let st = state_mutex.lock().unwrap();
                st.held_keys.contains(&Key::Return)
            };
            if !still_held || wait_start.elapsed() >= HELD_RELEASE_TIMEOUT {
                break;
            }
            thread::sleep(Duration::from_micros(100));
        }
    }

    // 2. switch_layout_to already polled until the layout change took effect,
    //    and the text below is layout-independent anyway.

    // 3. Gate the listener now that we are about to inject our own events.
    injecting.store(true, Ordering::Relaxed);

    let buf = {
        let st = state_mutex.lock().unwrap();
        st.buffered_keys.clone()
    };

    // +1 for the terminator the user physically typed.
    let delete_count = erase + 1 + buf.len();
    let mut inputs: Vec<INPUT> = Vec::with_capacity((delete_count + text.len() + 1) * 2);
    for _ in 0..delete_count {
        press(VK_BACK as u16, &mut inputs);
    }
    type_text(&text, &mut inputs);
    if needs_return {
        press(VK_RETURN as u16, &mut inputs);
    } else {
        type_text(" ", &mut inputs);
    }
    send(&mut inputs);

    // Keys the user managed to type while we were replacing: replayed as keys
    // (they are physical key positions, not text) once the word is back.
    if !buf.is_empty() {
        let mut replay: Vec<INPUT> = Vec::with_capacity(buf.len() * 2);
        for k in &buf {
            if let Some(vk) = vk_of(*k) {
                press(vk, &mut replay);
            }
        }
        send(&mut replay);
    }

    thread::sleep(INJECT_SETTLE);
    let mut st = state_mutex.lock().unwrap();
    st.keys = buf;
    st.buffered_keys.clear();
    st.is_replacing = false;
    injecting.store(false, Ordering::Relaxed);
}

/// Virtual-key code for a key we may have to replay. A VK is a key *position*,
/// so replaying one reproduces the physical key the user pressed whatever the
/// active layout is; letters and digits share their uppercase ASCII value.
/// `None` for anything the word buffer never holds.
fn vk_of(key: Key) -> Option<u16> {
    match key {
        Key::Space => Some(VK_SPACE as u16),
        Key::Return => Some(VK_RETURN as u16),
        other => key_to_english_char(other).map(|c| c.to_ascii_uppercase() as u16),
    }
}
