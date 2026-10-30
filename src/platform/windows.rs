use super::engine::{self, Engine, Plan, Platform};
use super::textkeys;
use crate::dictionary::Dict;
use crate::types::{AppControl, Language};
use rdev::Key;
use std::cell::RefCell;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use winapi::ctypes::c_int;
use winapi::shared::minwindef::{LPARAM, LRESULT, WPARAM};
use winapi::shared::windef::HHOOK;
use winapi::um::libloaderapi::GetModuleHandleW;
use winapi::um::winuser::{
    CallNextHookEx, DispatchMessageW, GetAsyncKeyState, GetKeyState, GetMessageW, SendInput,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HC_ACTION, INPUT, INPUT_KEYBOARD,
    KBDLLHOOKSTRUCT, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, LLKHF_EXTENDED,
    LLKHF_INJECTED, MSG, VK_BACK, VK_CAPITAL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN,
    VK_RCONTROL, VK_RETURN, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT, VK_SPACE, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_RBUTTONDOWN,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN,
};
type Typed = engine::Typed<Key>;
const RECAST_EVENT: usize = 0x5243_5354;

thread_local! {
    // Low-level hooks run on the thread that installed them. Keeping the engine
    // here avoids a mutable global callback or extending its lifetime forever.
    static CAPTURE_ENGINE: RefCell<Option<Arc<Engine<Windows>>>> = const { RefCell::new(None) };
}

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
    fn is_line_break(key: Key) -> bool {
        key == Key::Return
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
    fn append_retype(text: &mut String, tail: String) {
        text.push_str(&tail);
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
    fn is_own_focus(focus: &Focus) -> bool {
        let mut pid = 0;
        unsafe {
            winapi::um::winuser::GetWindowThreadProcessId(*focus as _, &mut pid);
        }
        pid == std::process::id()
    }
    fn selection(focus: &Focus) -> Option<engine::Selection> {
        super::windows_selection::selection(*focus)
    }
    fn replace_selection(
        engine: &Engine<Self>,
        focus: &Focus,
        expected: &engine::Selection,
        text: &str,
        generation: u64,
    ) -> Option<engine::Selection> {
        super::windows_selection::replace(engine, *focus, expected, text, generation)
    }
    fn app_id(focus: &Focus) -> Option<String> {
        use winapi::um::{
            handleapi::CloseHandle, processthreadsapi::OpenProcess,
            winbase::QueryFullProcessImageNameW, winnt::PROCESS_QUERY_LIMITED_INFORMATION,
            winuser::GetWindowThreadProcessId,
        };
        unsafe {
            let mut pid = 0;
            GetWindowThreadProcessId(*focus as _, &mut pid);
            if pid == 0 {
                return None;
            }
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return None;
            }
            let mut path = vec![0u16; 32768];
            let mut len = path.len() as u32;
            let ok = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len);
            CloseHandle(process);
            if ok == 0 {
                return None;
            }
            let path = String::from_utf16(&path[..len as usize]).ok()?;
            path.rsplit('\\')
                .next()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        }
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
    super::start_background_tasks(&control);
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
    let engine = Engine::<Windows>::new(en_dict, he_dict, control.clone(), AtomicBool::new(false));
    if let Err(err) = listen(engine, &control) {
        let _ = writeln!(
            std::io::stderr(),
            "Error while listening for keyboard events: {err}"
        );
    }
}

struct Hook(HHOOK);
impl Drop for Hook {
    fn drop(&mut self) {
        unsafe { UnhookWindowsHookEx(self.0) };
    }
}

fn listen(engine: Arc<Engine<Windows>>, control: &AppControl) -> std::io::Result<()> {
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    if module.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let install = |kind, callback| {
        let hook = unsafe { SetWindowsHookExW(kind, Some(callback), module, 0) };
        if hook.is_null() {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(Hook(hook))
        }
    };
    let _keyboard = install(
        WH_KEYBOARD_LL,
        keyboard_callback as unsafe extern "system" fn(_, _, _) -> _,
    )?;
    let _mouse = install(
        WH_MOUSE_LL,
        mouse_callback as unsafe extern "system" fn(_, _, _) -> _,
    )?;
    engine.caps_lock_changed(unsafe { GetKeyState(VK_CAPITAL) } & 1 != 0);
    engine.sync_modifiers(modifier_snapshot(|key| unsafe { GetAsyncKeyState(key) }));
    CAPTURE_ENGINE.with(|slot| *slot.borrow_mut() = Some(engine));
    control.listener_ready.store(true, Ordering::Relaxed);
    let result = loop {
        let mut message: MSG = unsafe { std::mem::zeroed() };
        match unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } {
            -1 => break Err(std::io::Error::last_os_error()),
            0 => break Ok(()),
            _ => unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            },
        }
    };
    control.listener_ready.store(false, Ordering::Relaxed);
    CAPTURE_ENGINE.with(|slot| slot.borrow_mut().take());
    result
}

/// Seed modifiers pressed before the hook existed without inventing tap events.
/// Query only at startup: a hook callback runs before its key's async state updates.
fn modifier_snapshot(mut state: impl FnMut(c_int) -> i16) -> [(Key, bool); 8] {
    [
        (Key::ShiftLeft, VK_LSHIFT),
        (Key::ShiftRight, VK_RSHIFT),
        (Key::ControlLeft, VK_LCONTROL),
        (Key::ControlRight, VK_RCONTROL),
        (Key::Alt, VK_LMENU),
        (Key::AltGr, VK_RMENU),
        (Key::MetaLeft, VK_LWIN),
        (Key::MetaRight, VK_RWIN),
    ]
    .map(|(key, vk)| (key, state(vk) < 0))
}

#[derive(Debug, PartialEq)]
enum CapturedKey {
    Press(Key),
    Release(Key),
    External,
}

fn decode_keyboard(message: WPARAM, event: &KBDLLHOOKSTRUCT) -> Option<CapturedKey> {
    let pressed = match message as u32 {
        WM_KEYDOWN | WM_SYSKEYDOWN => true,
        WM_KEYUP | WM_SYSKEYUP => false,
        _ => return None,
    };
    if event.flags & LLKHF_INJECTED != 0 {
        // Our events must be ignored even after the injection guard is cleared.
        // Other automation can edit text we did not observe as physical input.
        return (event.dwExtraInfo != RECAST_EVENT).then_some(CapturedKey::External);
    }
    let key = key_from_code(event.vkCode, event.scanCode, event.flags);
    Some(if pressed {
        CapturedKey::Press(key)
    } else {
        CapturedKey::Release(key)
    })
}

unsafe extern "system" fn keyboard_callback(code: c_int, message: WPARAM, data: LPARAM) -> LRESULT {
    if code == HC_ACTION {
        if let Some(event) = decode_keyboard(message, &*(data as *const KBDLLHOOKSTRUCT)) {
            CAPTURE_ENGINE.with(|slot| {
                if let Some(engine) = slot.borrow().as_ref() {
                    match event {
                        CapturedKey::Press(key) => engine.key_press(key),
                        CapturedKey::Release(key) => engine.key_release(key),
                        CapturedKey::External => engine.forget_everything(),
                    }
                }
            });
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, message, data)
}

unsafe extern "system" fn mouse_callback(code: c_int, message: WPARAM, data: LPARAM) -> LRESULT {
    if code == HC_ACTION
        && matches!(
            message as u32,
            WM_LBUTTONDOWN | WM_MBUTTONDOWN | WM_RBUTTONDOWN | WM_XBUTTONDOWN
        )
    {
        CAPTURE_ENGINE.with(|slot| {
            if let Some(engine) = slot.borrow().as_ref() {
                engine.mouse_click();
            }
        });
    }
    CallNextHookEx(std::ptr::null_mut(), code, message, data)
}

/// Preserve rdev's physical-key vocabulary without discarding native hook flags.
fn key_from_code(code: u32, scan: u32, flags: u32) -> Key {
    use Key as K;
    match code {
        0x10 => {
            if scan == 0x36 {
                K::ShiftRight
            } else {
                K::ShiftLeft
            }
        }
        0x11 => {
            if flags & LLKHF_EXTENDED != 0 {
                K::ControlRight
            } else {
                K::ControlLeft
            }
        }
        0x12 => {
            if flags & LLKHF_EXTENDED != 0 {
                K::AltGr
            } else {
                K::Alt
            }
        }
        0xa0 => K::ShiftLeft,
        0xa1 => K::ShiftRight,
        0xa2 => K::ControlLeft,
        0xa3 => K::ControlRight,
        0xa4 => K::Alt,
        0xa5 => K::AltGr,
        0x08 => K::Backspace,
        0x09 => K::Tab,
        0x0d => K::Return,
        0x13 => K::Pause,
        0x14 => K::CapsLock,
        0x1b => K::Escape,
        0x20 => K::Space,
        0x21 => K::PageUp,
        0x22 => K::PageDown,
        0x23 => K::End,
        0x24 => K::Home,
        0x25 => K::LeftArrow,
        0x26 => K::UpArrow,
        0x27 => K::RightArrow,
        0x28 => K::DownArrow,
        0x2c => K::PrintScreen,
        0x2d => K::Insert,
        0x2e => K::Delete,
        0x30 => K::Num0,
        0x31 => K::Num1,
        0x32 => K::Num2,
        0x33 => K::Num3,
        0x34 => K::Num4,
        0x35 => K::Num5,
        0x36 => K::Num6,
        0x37 => K::Num7,
        0x38 => K::Num8,
        0x39 => K::Num9,
        0x41 => K::KeyA,
        0x42 => K::KeyB,
        0x43 => K::KeyC,
        0x44 => K::KeyD,
        0x45 => K::KeyE,
        0x46 => K::KeyF,
        0x47 => K::KeyG,
        0x48 => K::KeyH,
        0x49 => K::KeyI,
        0x4a => K::KeyJ,
        0x4b => K::KeyK,
        0x4c => K::KeyL,
        0x4d => K::KeyM,
        0x4e => K::KeyN,
        0x4f => K::KeyO,
        0x50 => K::KeyP,
        0x51 => K::KeyQ,
        0x52 => K::KeyR,
        0x53 => K::KeyS,
        0x54 => K::KeyT,
        0x55 => K::KeyU,
        0x56 => K::KeyV,
        0x57 => K::KeyW,
        0x58 => K::KeyX,
        0x59 => K::KeyY,
        0x5a => K::KeyZ,
        0x5b => K::MetaLeft,
        0x5c => K::MetaRight,
        0x60 => K::Kp0,
        0x61 => K::Kp1,
        0x62 => K::Kp2,
        0x63 => K::Kp3,
        0x64 => K::Kp4,
        0x65 => K::Kp5,
        0x66 => K::Kp6,
        0x67 => K::Kp7,
        0x68 => K::Kp8,
        0x69 => K::Kp9,
        0x6a => K::KpMultiply,
        0x6b => K::KpPlus,
        0x6d => K::KpMinus,
        0x6e => K::KpDelete,
        0x6f => K::KpDivide,
        0x70 => K::F1,
        0x71 => K::F2,
        0x72 => K::F3,
        0x73 => K::F4,
        0x74 => K::F5,
        0x75 => K::F6,
        0x76 => K::F7,
        0x77 => K::F8,
        0x78 => K::F9,
        0x79 => K::F10,
        0x7a => K::F11,
        0x7b => K::F12,
        0x90 => K::NumLock,
        0x91 => K::ScrollLock,
        0xba => K::SemiColon,
        0xbb => K::Equal,
        0xbc => K::Comma,
        0xbd => K::Minus,
        0xbe => K::Dot,
        0xbf => K::Slash,
        0xc0 => K::BackQuote,
        0xdb => K::LeftBracket,
        0xdc => K::BackSlash,
        0xdd => K::RightBracket,
        0xde => K::Quote,
        0xe2 => K::IntlBackslash,
        _ => K::Unknown(code),
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
            dwExtraInfo: RECAST_EVENT,
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
    if terminator == Some(Key::Return) {
        return None;
    }
    let gaps = crate::timing::injection();
    if !engine.replacement_valid(generation) {
        return None;
    }
    engine.injector.store(true, Ordering::Relaxed);
    let buf = engine.buffered();
    let mut inputs = replacement_inputs(erase, &text, terminator, &buf);
    if !engine.replacement_valid(generation) {
        return None;
    }
    // Deletion, replacement, and the deleted suffix must be submitted together.
    // A cancellation between separate sends could otherwise lose that suffix.
    send(&mut inputs)?;

    crate::timing::pause(gaps.settle);
    Some(buf)
}
fn replacement_inputs(
    erase: usize,
    text: &str,
    terminator: Option<Key>,
    buffered: &[Typed],
) -> Vec<INPUT> {
    let delete_count = erase + buffered.len();
    let mut inputs = Vec::with_capacity((delete_count + text.len() + 1) * 2 + buffered.len() * 4);
    for _ in 0..delete_count {
        press(VK_BACK as u16, &mut inputs);
    }
    type_text(text, &mut inputs);
    // Explicit offers already include any tracked trailing spaces.
    if terminator.is_some() {
        type_text(" ", &mut inputs);
    }
    // Buffered keys are physical positions. Preserve their Shift state in the
    // same transaction that erases their already-visible original characters.
    for t in buffered {
        let Some(vk) = vk_of(t.key) else { continue };
        if t.shift {
            inputs.push(key_input(VK_SHIFT as u16, None, false));
            press(vk, &mut inputs);
            inputs.push(key_input(VK_SHIFT as u16, None, true));
        } else {
            press(vk, &mut inputs);
        }
    }
    inputs
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
    use super::*;

    #[test]
    fn startup_modifier_snapshot_preserves_sides_and_ignores_recent_press_bits() {
        let expected = [
            (Key::ShiftLeft, VK_LSHIFT),
            (Key::ShiftRight, VK_RSHIFT),
            (Key::ControlLeft, VK_LCONTROL),
            (Key::ControlRight, VK_RCONTROL),
            (Key::Alt, VK_LMENU),
            (Key::AltGr, VK_RMENU),
            (Key::MetaLeft, VK_LWIN),
            (Key::MetaRight, VK_RWIN),
        ];
        for (held, vk) in expected {
            for held_state in [i16::MIN, i16::MIN | 1] {
                let mut queried = Vec::new();
                let snapshot = modifier_snapshot(|key| {
                    queried.push(key);
                    if key == vk {
                        held_state
                    } else {
                        1
                    }
                });
                assert_eq!(queried, expected.map(|(_, vk)| vk));
                assert_eq!(snapshot, expected.map(|(key, _)| (key, key == held)));
            }
        }
    }

    fn native_key(vk: u32, scan: u32, flags: u32, extra: usize) -> KBDLLHOOKSTRUCT {
        KBDLLHOOKSTRUCT {
            vkCode: vk,
            scanCode: scan,
            flags,
            time: 0,
            dwExtraInfo: extra,
        }
    }

    #[test]
    fn native_capture_distinguishes_our_input_from_physical_typing() {
        for (message, expected) in [
            (WM_KEYDOWN, CapturedKey::Press(Key::ShiftRight)),
            (WM_SYSKEYDOWN, CapturedKey::Press(Key::ShiftRight)),
            (WM_KEYUP, CapturedKey::Release(Key::ShiftRight)),
            (WM_SYSKEYUP, CapturedKey::Release(Key::ShiftRight)),
        ] {
            let event = native_key(0xa1, 0x36, 0, 0);
            assert_eq!(decode_keyboard(message as WPARAM, &event), Some(expected));
            let own = native_key(0xa1, 0x36, LLKHF_INJECTED, RECAST_EVENT);
            assert_eq!(decode_keyboard(message as WPARAM, &own), None);
            let external = native_key(0xa1, 0x36, LLKHF_INJECTED, 0);
            assert_eq!(
                decode_keyboard(message as WPARAM, &external),
                Some(CapturedKey::External)
            );
        }
        // Repeated physical presses remain visible; matching extra information
        // on a physical event alone must never cause it to be discarded.
        let physical = native_key(0x41, 0x1e, 0, RECAST_EVENT);
        for _ in 0..3 {
            assert_eq!(
                decode_keyboard(WM_KEYDOWN as WPARAM, &physical),
                Some(CapturedKey::Press(Key::KeyA))
            );
        }
        assert_eq!(decode_keyboard(0, &physical), None);
        for unicode in [None, Some(0x05e9)] {
            for up in [false, true] {
                let input = key_input(VK_BACK as u16, unicode, up);
                assert_eq!(unsafe { input.u.ki().dwExtraInfo }, RECAST_EVENT);
            }
        }
    }

    #[test]
    fn native_mapping_preserves_shortcut_sides_and_text_positions() {
        for (vk, scan, flags, expected) in [
            (0x10, 0x2a, 0, Key::ShiftLeft),
            (0x10, 0x36, 0, Key::ShiftRight),
            (0x11, 0x1d, 0, Key::ControlLeft),
            (0x11, 0x1d, LLKHF_EXTENDED, Key::ControlRight),
            (0x12, 0x38, 0, Key::Alt),
            (0x12, 0x38, LLKHF_EXTENDED, Key::AltGr),
            (0xa0, 0x2a, 0, Key::ShiftLeft),
            (0xa1, 0x36, 0, Key::ShiftRight),
            (0xa2, 0x1d, 0, Key::ControlLeft),
            (0xa3, 0x1d, LLKHF_EXTENDED, Key::ControlRight),
            (0x5b, 0, LLKHF_EXTENDED, Key::MetaLeft),
            (0x5c, 0, LLKHF_EXTENDED, Key::MetaRight),
            (0xff, 0, 0, Key::Unknown(0xff)),
        ] {
            assert_eq!(key_from_code(vk, scan, flags), expected);
        }
        for text in "abcdefghijklmnopqrstuvwxyz0123456789`-=[]\\;',./ ".chars() {
            let (expected, _) = crate::keymap::english_char_to_key(text).unwrap();
            let vk = vk_of(expected).unwrap();
            assert_eq!(key_from_code(vk.into(), 0, 0), expected, "{text}");
        }
        for (vk, expected) in [
            (0x08, Key::Backspace),
            (0x09, Key::Tab),
            (0x0d, Key::Return),
            (0x1b, Key::Escape),
            (0x23, Key::End),
            (0x24, Key::Home),
            (0x25, Key::LeftArrow),
            (0x26, Key::UpArrow),
            (0x27, Key::RightArrow),
            (0x28, Key::DownArrow),
            (0x2d, Key::Insert),
            (0x2e, Key::Delete),
        ] {
            assert_eq!(key_from_code(vk, 0, 0), expected);
        }
    }

    #[test]
    fn replacement_transaction_includes_deleted_buffered_typing() {
        let buffered = [
            Typed {
                key: Key::KeyA,
                shift: true,
            },
            Typed {
                key: Key::SemiColon,
                shift: false,
            },
            Typed {
                key: Key::Space,
                shift: false,
            },
        ];
        for terminator in [None, Some(Key::Space)] {
            let inputs = replacement_inputs(2, "ש🙂", terminator, &buffered);
            let events: Vec<_> = inputs
                .iter()
                .map(|input| unsafe {
                    let key = input.u.ki();
                    assert_eq!(key.dwExtraInfo, RECAST_EVENT);
                    (key.wVk, key.wScan, key.dwFlags)
                })
                .collect();
            // The exact payload passed to the single SendInput call contains
            // every erased buffered character, including Shift and punctuation.
            let mut expected =
                [(VK_BACK as u16, 0, 0), (VK_BACK as u16, 0, KEYEVENTF_KEYUP)].repeat(5);
            for unit in "ש🙂"
                .encode_utf16()
                .chain(terminator.map(|_| u16::from(b' ')))
            {
                expected.push((0, unit, KEYEVENTF_UNICODE));
                expected.push((0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
            }
            expected.extend([
                (VK_SHIFT as u16, 0, 0),
                (0x41, 0, 0),
                (0x41, 0, KEYEVENTF_KEYUP),
                (VK_SHIFT as u16, 0, KEYEVENTF_KEYUP),
                (0xba, 0, 0),
                (0xba, 0, KEYEVENTF_KEYUP),
                (VK_SPACE as u16, 0, 0),
                (VK_SPACE as u16, 0, KEYEVENTF_KEYUP),
            ]);
            assert_eq!(events, expected);
        }
    }

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
