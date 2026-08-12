//! Windows: `rdev::listen` capture, `SendInput` injection, and the tray/TUI
//! startup path.
//!
//! The state machine is [`crate::platform::engine`] and everything about keys
//! and text is [`crate::platform::textkeys`], shared with macOS — both capture
//! `rdev::Key` and both insert corrections as text. What is left here is the
//! listener, the injection, and the console reattachment that lets a
//! GUI-subsystem build print anything at all.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use rdev::{listen, Event, EventType, Key};
use winapi::ctypes::c_int;
use winapi::um::winuser::{
    SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VK_BACK,
    VK_RETURN, VK_SHIFT, VK_SPACE,
};

use super::engine::{Engine, Plan, Platform, Typed};
use super::textkeys;
use crate::dictionary::Dict;
use crate::keymap::key_to_english_char;
use crate::types::{AppControl, Language};

pub struct Windows;

impl Platform for Windows {
    type Key = Key;
    type Retype = String;
    /// The re-entry gate: while it is set the listener discards every event,
    /// because the events arriving are ours.
    type Injector = Arc<AtomicBool>;

    const SHIFT_LEFT: Key = textkeys::SHIFT_LEFT;
    const SHIFT_RIGHT: Key = textkeys::SHIFT_RIGHT;
    const CTRL_LEFT: Key = textkeys::CTRL_LEFT;
    const CTRL_RIGHT: Key = textkeys::CTRL_RIGHT;
    const CAPS_LOCK: Key = textkeys::CAPS_LOCK;
    const BACKSPACE: Key = textkeys::BACKSPACE;

    fn is_terminator(key: Key) -> bool {
        textkeys::is_terminator(key)
    }
    fn is_reset(key: Key) -> bool {
        textkeys::is_reset(key)
    }
    fn english_char(key: Key, shift: bool) -> Option<char> {
        textkeys::english_char(key, shift)
    }
    fn english_char_plain(key: Key) -> Option<char> {
        textkeys::english_char_plain(key)
    }
    fn hebrew_char(key: Key) -> Option<char> {
        textkeys::hebrew_char(key)
    }
    fn retype_original(keys: &[Typed<Key>], lang: Language) -> String {
        super::engine::reading::<Self>(keys, lang)
    }
    fn retype_layout(_keys: &[Typed<Key>], text: &str, _lang: Language) -> Option<String> {
        textkeys::retype_text(text)
    }
    fn retype_text(text: &str) -> Option<String> {
        textkeys::retype_text(text)
    }
    fn retype_len(retype: &String) -> usize {
        textkeys::retype_len(retype)
    }
    fn buffer_after(retype: &String) -> Vec<Typed<Key>> {
        textkeys::buffer_after(retype)
    }
    fn injecting_flag(injector: &Self::Injector) -> Option<&AtomicBool> {
        Some(injector)
    }

    fn inject(engine: &Engine<Self>, plan: Plan<Self>) -> Vec<Typed<Key>> {
        let gaps = crate::timing::injection();

        // 1. The corrected word goes in as text, so the only real key pressed
        //    is Return (a space rides along inside the text instead). Wait for
        //    the user to lift it first: a synthetic press of a still-held key
        //    is swallowed as a duplicate. The gate is still open here, so
        //    release events from the listener keep updating `held_keys`.
        if plan.terminator == Some(Key::Return) {
            engine.wait_for_release(&[Key::Return], gaps.held_release_timeout);
        }

        // 2. `switch_layout_to` already polled until the layout change took
        //    effect, and the text below is layout-independent anyway.

        // 3. Gate the listener now that we are about to inject our own events.
        engine.injector.store(true, Ordering::Relaxed);

        let buffered = engine.buffered();

        let delete_count = plan.erase + buffered.len();
        let mut inputs: Vec<INPUT> = Vec::with_capacity((delete_count + plan.retype.len() + 1) * 2);
        for _ in 0..delete_count {
            press(VK_BACK as u16, &mut inputs);
        }
        type_text(&plan.retype, &mut inputs);
        match plan.terminator {
            Some(Key::Return) => press(VK_RETURN as u16, &mut inputs),
            Some(_) => type_text(" ", &mut inputs),
            // A completion ends mid-word: no terminator, no trailing space.
            None => {}
        }
        send(&mut inputs);

        // Keys the user managed to type while we were replacing: replayed as
        // keys (they are physical key positions, not text) once the word is
        // back.
        if !buffered.is_empty() {
            let mut replay: Vec<INPUT> = Vec::with_capacity(buffered.len() * 4);
            for t in &buffered {
                let Some(vk) = vk_of(t.key) else { continue };
                // Replay the shift too, or a capital comes back lowercase.
                if t.shift {
                    replay.push(key_input(VK_SHIFT as u16, None, false));
                    press(vk, &mut replay);
                    replay.push(key_input(VK_SHIFT as u16, None, true));
                } else {
                    press(vk, &mut replay);
                }
            }
            send(&mut replay);
        }

        crate::timing::pause(gaps.settle);
        buffered
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

// ─────────────────────────────────────────────────────────────────────────────
// Startup and capture
// ─────────────────────────────────────────────────────────────────────────────

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

    let wide =
        |s: &str| -> Vec<u16> { std::ffi::OsStr::new(s).encode_wide().chain(once(0)).collect() };

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
pub fn start(en: Dict, he: Dict, control: Arc<AppControl>, with_gui: bool) {
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

/// Every write below goes through `writeln!` with its result dropped, never
/// `println!`/`eprintln!`: a release build sets `windows_subsystem = "windows"`,
/// so there is no console, the macro's write returns `Err`, and the macro
/// PANICS. This function runs on the listener thread, so that panic would
/// silently kill keyboard capture while the tray kept running — the app would
/// look alive and correct nothing.
pub fn run(en_dict: Dict, he_dict: Dict, control: Arc<AppControl>) {
    let injecting = Arc::new(AtomicBool::new(false));
    let engine = Engine::<Windows>::new(en_dict, he_dict, control, Arc::clone(&injecting));

    let callback = move |event: Event| {
        // Ignore the key events we generate ourselves; otherwise the listener
        // treats them as user input and interferes with the replacement.
        if injecting.load(Ordering::Relaxed) {
            return;
        }
        match event.event_type {
            EventType::KeyPress(key) => engine.key_press(key),
            EventType::KeyRelease(key) => engine.key_release(key),
            EventType::ButtonPress(_) => engine.mouse_click(),
            _ => {}
        }
    };

    // Nothing is printed on the way up: the banner has already said hello on a
    // terminal launch, and under the Scheduled Task there is no console to say
    // it to. Only a failure is worth a line — Linux and macOS are silent here
    // for the same reason.
    if let Err(err) = listen(callback) {
        let _ = writeln!(
            std::io::stderr(),
            "Error while listening for keyboard events: {err:?}"
        );
    }
}
