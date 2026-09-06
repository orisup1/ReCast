use super::engine::{self, Engine, Plan, Platform};
use super::textkeys;
use crate::dictionary::Dict;
use crate::types::{AppControl, Language};
use rdev::{listen, Event, EventType, Key};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use winapi::ctypes::c_int;
use winapi::um::winuser::{
    SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VK_BACK,
    VK_RETURN, VK_SHIFT, VK_SPACE,
};
type Typed = engine::Typed<Key>;

pub struct Windows;
impl Platform for Windows {
    type Key = Key;
    type Retype = String;
    type Injector = AtomicBool;
    type Focus = Focus;
    const REQUIRES_FOCUS: bool = true;
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
    fn is_modifier(key: Key) -> bool {
        textkeys::is_modifier(key)
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
    fn retype_original(keys: &[Typed], lang: Language) -> String {
        engine::reading::<Self>(keys, lang)
    }
    fn retype_layout(_: &[Typed], text: &str, _: Language) -> Option<String> {
        textkeys::retype_text(text)
    }
    fn retype_text(text: &str) -> Option<String> {
        textkeys::retype_text(text)
    }
    fn retype_len(text: &String) -> usize {
        textkeys::retype_len(text)
    }
    fn buffer_after(text: &String) -> Vec<Typed> {
        textkeys::buffer_after(text)
    }
    fn injecting_flag(injector: &AtomicBool) -> Option<&AtomicBool> {
        Some(injector)
    }
    fn focus() -> Option<Focus> {
        focused_target()
    }
    fn inject(engine: &Engine<Self>, plan: Plan<Self>, generation: u64) -> Option<Vec<Typed>> {
        inject(engine, plan, generation)
    }
}
pub fn attach_parent_console() {
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    use winapi::um::consoleapi::{GetConsoleMode, SetConsoleMode};
    use winapi::um::fileapi::{CreateFileW, OPEN_EXISTING};
    use winapi::um::handleapi::INVALID_HANDLE_VALUE;
    use winapi::um::processenv::{GetStdHandle, SetStdHandle};
    use winapi::um::winbase::{STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    use winapi::um::wincon::{
        AttachConsole, ATTACH_PARENT_PROCESS, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    };
    use winapi::um::winnt::{FILE_SHARE_READ, FILE_SHARE_WRITE, GENERIC_READ, GENERIC_WRITE};

    let wide = |s: &str| -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(once(0))
            .collect()
    };

    unsafe {
        // Preserve pipes/files supplied by a CLI caller before attaching.
        let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        let stderr = GetStdHandle(STD_ERROR_HANDLE);
        let stdin = GetStdHandle(STD_INPUT_HANDLE);
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
            for (kind, original) in [(STD_OUTPUT_HANDLE, stdout), (STD_ERROR_HANDLE, stderr)] {
                SetStdHandle(
                    kind,
                    if original.is_null() || original == INVALID_HANDLE_VALUE {
                        conout
                    } else {
                        original
                    },
                );
            }
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
            SetStdHandle(
                STD_INPUT_HANDLE,
                if stdin.is_null() || stdin == INVALID_HANDLE_VALUE {
                    conin
                } else {
                    stdin
                },
            );
        }
    }
}

/// Full Windows startup. Owns everything that used to live in `main`'s Windows
/// `cfg` block: run the keyboard listener on a background thread and hand the
/// main thread to the tray (or the TUI with `--gui`). Keeping it here means
/// changes to the Windows launch path can't touch the Linux or macOS paths.
pub fn start(en: Dict, he: Dict, control: Arc<AppControl>, with_gui: bool) {
    super::start_background_tasks();
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
    let engine = Engine::<Windows>::new(en_dict, he_dict, control, AtomicBool::new(false));
    let callback = move |event: Event| {
        if let EventType::ButtonPress(_) = event.event_type {
            engine.mouse_click();
            return;
        }
        if engine.injector.load(Ordering::Relaxed) {
            return;
        }
        match event.event_type {
            EventType::KeyPress(key) => engine.key_press(key),
            EventType::KeyRelease(key) => engine.key_release(key),
            _ => {}
        }
    };
    if let Err(err) = listen(callback) {
        let _ = writeln!(
            std::io::stderr(),
            "Error while listening for keyboard events: {err:?}"
        );
    }
}
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
fn send(inputs: &mut [INPUT]) -> Option<()> {
    if inputs.is_empty() {
        return Some(());
    }
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_mut_ptr(),
            std::mem::size_of::<INPUT>() as c_int,
        )
    };
    (sent as usize == inputs.len()).then_some(())
}

fn inject(engine: &Engine<Windows>, plan: Plan<Windows>, generation: u64) -> Option<Vec<Typed>> {
    let Plan {
        erase,
        retype: text,
        terminator,
    } = plan;
    let gaps = crate::timing::injection();
    if terminator == Some(Key::Return) {
        engine.wait_for_release(&[Key::Return], gaps.held_release_timeout);
    }
    if !engine.replacement_valid(generation) {
        return None;
    }
    engine.injector.store(true, Ordering::Relaxed);
    let buf = engine.buffered();
    let delete_count = erase + buf.len();
    let mut inputs: Vec<INPUT> = Vec::with_capacity((delete_count + text.len() + 1) * 2);
    for _ in 0..delete_count {
        press(VK_BACK as u16, &mut inputs);
    }
    type_text(&text, &mut inputs);
    match terminator {
        Some(Key::Return) => press(VK_RETURN as u16, &mut inputs),
        Some(_) => type_text(" ", &mut inputs),
        // A completion ends mid-word: no terminator, no trailing space.
        None => {}
    }
    if !engine.replacement_valid(generation) {
        return None;
    }
    send(&mut inputs)?;

    // Keys the user managed to type while we were replacing: replayed as keys
    // (they are physical key positions, not text) once the word is back.
    if !buf.is_empty() {
        let mut replay: Vec<INPUT> = Vec::with_capacity(buf.len() * 4);
        for t in &buf {
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
        if !engine.replacement_valid(generation) {
            return None;
        }
        send(&mut replay)?;
    }

    crate::timing::pause(gaps.settle);
    Some(buf)
}
fn vk_of(key: Key) -> Option<u16> {
    use winapi::um::winuser::{
        VK_OEM_1, VK_OEM_2, VK_OEM_3, VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA,
        VK_OEM_MINUS, VK_OEM_PERIOD, VK_OEM_PLUS,
    };
    match key {
        Key::Space => Some(VK_SPACE as u16),
        Key::Return => Some(VK_RETURN as u16),
        other => Some(match textkeys::english_char_plain(other)? {
            ';' => VK_OEM_1 as u16,
            '/' => VK_OEM_2 as u16,
            '`' => VK_OEM_3 as u16,
            '[' => VK_OEM_4 as u16,
            '\\' => VK_OEM_5 as u16,
            ']' => VK_OEM_6 as u16,
            '\'' => VK_OEM_7 as u16,
            ',' => VK_OEM_COMMA as u16,
            '-' => VK_OEM_MINUS as u16,
            '.' => VK_OEM_PERIOD as u16,
            '=' => VK_OEM_PLUS as u16,
            c if c.is_ascii_alphanumeric() => c.to_ascii_uppercase() as u16,
            _ => return None,
        }),
    }
}

type Focus = usize;
fn focused_target() -> Option<Focus> {
    use winapi::um::winuser::{GetGUIThreadInfo, GUITHREADINFO};
    unsafe {
        let mut info: GUITHREADINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        // Thread zero asks about the foreground queue, not our tray thread.
        (GetGUIThreadInfo(0, &mut info) != 0 && !info.hwndFocus.is_null())
            .then_some(info.hwndFocus as usize)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn buffered_punctuation_uses_virtual_keys_not_ascii_control_codes() {
        for (text, expected) in [
            (';', 0xba),
            ('=', 0xbb),
            (',', 0xbc),
            ('-', 0xbd),
            ('.', 0xbe),
            ('/', 0xbf),
            ('`', 0xc0),
            ('[', 0xdb),
            ('\\', 0xdc),
            (']', 0xdd),
            ('\'', 0xde),
            ('a', 0x41),
            ('1', 0x31),
        ] {
            let (key, _) = crate::keymap::english_char_to_key(text).unwrap();
            assert_eq!(super::vk_of(key), Some(expected), "{text}");
        }
    }
}
