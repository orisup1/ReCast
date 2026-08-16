//! macOS: a raw `CGEventTap` on the main run loop, Unicode-string injection,
//! and the menubar startup path.
//!
//! The state machine is [`crate::platform::engine`] and everything about keys
//! and text is [`crate::platform::textkeys`], shared with Windows — both
//! capture `rdev::Key` and both insert corrections as text. What is left here
//! is the tap, the three macOS-only facts about it (it must live on the main
//! run loop, modifiers never arrive as key events, and secure input has to be
//! respected), and the injection.

use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use rdev::{simulate, EventType, Key};

use super::engine::{Engine, Plan, Platform, Typed};
use super::textkeys;
use crate::dictionary::Dict;
use crate::types::{AppControl, Language};

pub struct MacOs;

impl Platform for MacOs {
    type Key = Key;
    type Retype = String;
    /// The re-entry gate: while it is set the tap discards every event, because
    /// the events arriving are ours.
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
        //    release events from the tap keep updating `held_keys`.
        if plan.terminator == Some(Key::Return) {
            engine.wait_for_release(&[Key::Return], gaps.held_release_timeout);
        }

        // 2. `switch_layout_to` already polled until the new layout took
        //    effect, and the pasted text is layout-independent anyway.

        // 3. Gate the tap now that we are about to inject our own events.
        engine.injector.store(true, Ordering::Relaxed);

        let buffered = engine.buffered();

        // Press + release a single key with pacing macOS won't drop. Only the
        // backspaces and the odd replayed key go through this; the word itself
        // is one event.
        let tap_key = |k: Key| {
            let _ = simulate(&EventType::KeyPress(k));
            crate::timing::pause(gaps.press_gap);
            let _ = simulate(&EventType::KeyRelease(k));
            crate::timing::pause(gaps.inter_key_gap);
        };

        let delete_count = plan.erase + buffered.len();
        for _ in 0..delete_count {
            tap_key(Key::Backspace);
        }
        match plan.terminator {
            Some(Key::Return) => {
                paste_text(&plan.retype);
                tap_key(Key::Return);
            }
            // The trailing space is part of the same paste, so nothing has to
            // be pressed at all.
            Some(_) => paste_text(&format!("{} ", plan.retype)),
            // A completion ends mid-word: no terminator, no trailing space.
            None => paste_text(&plan.retype),
        }
        // Keys the user managed to type while we were replacing: replayed as
        // keys (they are physical key positions, not text) once the word is
        // back, with the shift the user held so a capital stays a capital.
        for t in buffered.iter() {
            if t.shift {
                let _ = simulate(&EventType::KeyPress(Key::ShiftLeft));
                crate::timing::pause(gaps.press_gap);
                tap_key(t.key);
                let _ = simulate(&EventType::KeyRelease(Key::ShiftLeft));
                crate::timing::pause(gaps.inter_key_gap);
            } else {
                tap_key(t.key);
            }
        }

        // The last injected key already paid `inter_key_gap`, and settling is
        // the same kind of wait for the same events — so only the difference is
        // owed.
        crate::timing::pause(gaps.settle.saturating_sub(gaps.inter_key_gap));
        buffered
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CoreGraphics / CoreFoundation FFI for direct CGEventTap on the main run loop.
//
// We can't use rdev::listen on macOS: it calls CFRunLoopRun() on the calling
// thread and adds the tap source to CFRunLoopGetCurrent(). When invoked from a
// background thread (because the tray owns the main thread), the tap runs on a
// run loop the OS doesn't expect, and on recent macOS versions the process is
// terminated after ~2s.
//
// Instead we attach the tap source to CFRunLoopGetMain() and let tao's NSApp
// event loop drive it. The callback fires on the main thread alongside menu
// events. No CFRunLoopRun needed here.
// ─────────────────────────────────────────────────────────────────────────────

type CFMachPortRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopMode = *const c_void;
type CGEventTapProxy = *mut c_void;
type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;
type CFIndex = isize;

const KCG_HID_EVENT_TAP: u32 = 0;
const KCG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const KCG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;

const KCG_EVENT_LEFT_MOUSE_DOWN: u32 = 1;
const KCG_EVENT_RIGHT_MOUSE_DOWN: u32 = 3;
const KCG_EVENT_KEY_DOWN: u32 = 10;
const KCG_EVENT_KEY_UP: u32 = 11;
/// Modifier keys — Shift, Ctrl, Caps Lock — are *not* delivered as key-down and
/// key-up on macOS. They arrive only as this event type, which is why it has to
/// be in the mask: without it the word buffer never learns that a shift was
/// held, and neither of the tap gestures (Right Shift to complete, Ctrl twice
/// to undo) can fire at all.
const KCG_EVENT_FLAGS_CHANGED: u32 = 12;
const KCG_EVENT_OTHER_MOUSE_DOWN: u32 = 25;

/// Caps Lock's flag. Unlike the others it is a latch: the bit *is* the state,
/// rather than saying whether a key is being held.
const FLAG_ALPHA_SHIFT: u64 = 0x0001_0000;

// Sent by the OS (not the user) when it forcibly disables our tap: either a
// callback ran too long (`ByTimeout`) or a security / user-input event tripped
// it (`ByUserInput`). A disabled tap delivers no further keystrokes, so the
// callback must re-enable the tap when it sees these types.
const KCG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const KCG_EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;

const EVENT_MASK: u64 = (1u64 << KCG_EVENT_LEFT_MOUSE_DOWN)
    | (1u64 << KCG_EVENT_RIGHT_MOUSE_DOWN)
    | (1u64 << KCG_EVENT_KEY_DOWN)
    | (1u64 << KCG_EVENT_KEY_UP)
    | (1u64 << KCG_EVENT_FLAGS_CHANGED)
    | (1u64 << KCG_EVENT_OTHER_MOUSE_DOWN);

const KCG_KEYBOARD_EVENT_KEYCODE: u32 = 9;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: unsafe extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    /// Which modifiers are down *after* the event. A `flagsChanged` event says
    /// which key changed but not in which direction, so this is what turns it
    /// back into a press or a release.
    fn CGEventGetFlags(event: CGEventRef) -> u64;

    // Text injection (see `paste_text`). A keyboard event carrying a Unicode
    // string inserts the whole string at once, independent of the active
    // layout — the OS treats it as typed text rather than key positions.
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        virtual_key: u16,
        key_down: bool,
    ) -> CGEventRef;
    fn CGEventKeyboardSetUnicodeString(
        event: CGEventRef,
        string_length: usize,
        unicode_string: *const u16,
    );
    fn CGEventPost(tap: u32, event: CGEventRef);
}

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    /// Whether some application has turned on secure event input — what a
    /// password field does while it has focus. Returns a Carbon `Boolean`
    /// (an unsigned char), so it is taken as `u8` rather than `bool`: any
    /// non-zero value is true, and only 0 and 1 would be sound as a Rust bool.
    fn IsSecureEventInputEnabled() -> u8;
}

/// Whether a password field (or anything else asking for secure input) has
/// focus right now.
///
/// While it does, ReCast stops looking at the keyboard entirely: the buffer is
/// dropped, nothing is checked, nothing is corrected. The tap is listen-only
/// and macOS already withholds the characters, but "we couldn't have read it
/// anyway" is a weaker promise than not being in the loop at all — and the
/// visible half matters too, since a correction firing inside a password field
/// would rewrite a password on the strength of a dictionary lookup.
///
/// Cheap enough to ask per keystroke: it reads a process-wide flag the window
/// server keeps, with no round trip.
fn secure_input_active() -> bool {
    unsafe { IsSecureEventInputEnabled() != 0 }
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(
        allocator: *mut c_void,
        port: CFMachPortRef,
        order: CFIndex,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFRunLoopMode);
    fn CFRunLoopGetMain() -> CFRunLoopRef;
    fn CFRelease(cf: *const c_void);
    static kCFRunLoopCommonModes: CFRunLoopMode;
}

// macOS virtual keycodes → rdev::Key. Mirrors rdev's private mapping (we can't
// access it from outside the crate) so the existing keymap.rs lookups keep
// working unchanged.
fn key_from_code(code: u16) -> Key {
    match code {
        0 => Key::KeyA,
        1 => Key::KeyS,
        2 => Key::KeyD,
        3 => Key::KeyF,
        4 => Key::KeyH,
        5 => Key::KeyG,
        6 => Key::KeyZ,
        7 => Key::KeyX,
        8 => Key::KeyC,
        9 => Key::KeyV,
        11 => Key::KeyB,
        12 => Key::KeyQ,
        13 => Key::KeyW,
        14 => Key::KeyE,
        15 => Key::KeyR,
        16 => Key::KeyY,
        17 => Key::KeyT,
        18 => Key::Num1,
        19 => Key::Num2,
        20 => Key::Num3,
        21 => Key::Num4,
        22 => Key::Num6,
        23 => Key::Num5,
        24 => Key::Equal,
        25 => Key::Num9,
        26 => Key::Num7,
        27 => Key::Minus,
        28 => Key::Num8,
        29 => Key::Num0,
        30 => Key::RightBracket,
        31 => Key::KeyO,
        32 => Key::KeyU,
        33 => Key::LeftBracket,
        34 => Key::KeyI,
        35 => Key::KeyP,
        36 => Key::Return,
        37 => Key::KeyL,
        38 => Key::KeyJ,
        39 => Key::Quote,
        40 => Key::KeyK,
        41 => Key::SemiColon,
        42 => Key::BackSlash,
        43 => Key::Comma,
        44 => Key::Slash,
        45 => Key::KeyN,
        46 => Key::KeyM,
        47 => Key::Dot,
        48 => Key::Tab,
        49 => Key::Space,
        50 => Key::BackQuote,
        51 => Key::Backspace,
        53 => Key::Escape,
        54 => Key::MetaRight,
        55 => Key::MetaLeft,
        56 => Key::ShiftLeft,
        57 => Key::CapsLock,
        58 => Key::Alt,
        59 => Key::ControlLeft,
        60 => Key::ShiftRight,
        62 => Key::ControlRight,
        63 => Key::Function,
        96 => Key::F5,
        97 => Key::F6,
        98 => Key::F7,
        99 => Key::F3,
        100 => Key::F8,
        101 => Key::F9,
        103 => Key::F11,
        109 => Key::F10,
        111 => Key::F12,
        118 => Key::F4,
        120 => Key::F2,
        122 => Key::F1,
        123 => Key::LeftArrow,
        124 => Key::RightArrow,
        125 => Key::DownArrow,
        126 => Key::UpArrow,
        other => Key::Unknown(other as u32),
    }
}

/// The engine, reachable from the tap callback — which is an `extern "C"`
/// function the OS calls and so can carry no state of its own.
static ENGINE: OnceLock<Arc<Engine<MacOs>>> = OnceLock::new();

/// Raw handle to our tap, stored so the callback can re-enable it if the OS
/// disables it. `CFMachPortRef` is a thread-safe Core Foundation type.
struct TapPort(CFMachPortRef);
unsafe impl Send for TapPort {}
unsafe impl Sync for TapPort {}
static TAP_PORT: OnceLock<TapPort> = OnceLock::new();

unsafe extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: u32,
    cg_event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    // The OS disables the tap on a callback timeout or certain input events.
    // Once disabled it delivers nothing further, so re-enable it immediately —
    // otherwise the app silently stops seeing the keyboard while the tray keeps
    // running. Handled before anything else, independent of the engine.
    if event_type == KCG_EVENT_TAP_DISABLED_BY_TIMEOUT
        || event_type == KCG_EVENT_TAP_DISABLED_BY_USER_INPUT
    {
        if let Some(port) = TAP_PORT.get() {
            CGEventTapEnable(port.0, true);
        }
        return cg_event;
    }

    let Some(engine) = ENGINE.get() else {
        return cg_event;
    };

    if engine.injector.load(Ordering::Relaxed) {
        return cg_event;
    }

    // A password field has focus: drop whatever is buffered and look away until
    // it doesn't.
    if secure_input_active() {
        engine.forget_everything();
        return cg_event;
    }

    match event_type {
        KCG_EVENT_KEY_DOWN => {
            let code = CGEventGetIntegerValueField(cg_event, KCG_KEYBOARD_EVENT_KEYCODE) as u16;
            engine.key_press(key_from_code(code));
        }
        KCG_EVENT_KEY_UP => {
            let code = CGEventGetIntegerValueField(cg_event, KCG_KEYBOARD_EVENT_KEYCODE) as u16;
            engine.key_release(key_from_code(code));
        }
        KCG_EVENT_FLAGS_CHANGED => {
            let code = CGEventGetIntegerValueField(cg_event, KCG_KEYBOARD_EVENT_KEYCODE) as u16;
            handle_flags_changed(engine, key_from_code(code), CGEventGetFlags(cg_event));
        }
        KCG_EVENT_LEFT_MOUSE_DOWN | KCG_EVENT_RIGHT_MOUSE_DOWN | KCG_EVENT_OTHER_MOUSE_DOWN => {
            engine.mouse_click();
        }
        _ => {}
    }

    cg_event
}

/// A modifier changed state. macOS never delivers these as key-down / key-up
/// (see [`KCG_EVENT_FLAGS_CHANGED`]), and the event says which key changed but
/// not in which direction — the flags say which modifiers are down afterwards,
/// so the direction is read back off them.
///
/// The documented `kCGEventFlagMask*` constants say "a shift is down" without
/// saying which one, so the *device-dependent* bits are what distinguish left
/// from right — and this whole feature is built on telling them apart.
fn handle_flags_changed(engine: &Arc<Engine<MacOs>>, key: Key, flags: u64) {
    if key == Key::CapsLock {
        // A latch rather than a held key: the flag *is* the state, so it is
        // assigned rather than fed through `key_press` — which toggles, and is
        // what Linux and Windows use because there Caps Lock arrives as an
        // ordinary key event. It never reaches `key_press` here, so the two
        // treatments cannot both apply.
        crate::types::lock_forgiving(&engine.state).caps_lock =
            flags & FLAG_ALPHA_SHIFT != 0;
        return;
    }
    let Some(bit) = device_flag(key) else {
        return;
    };
    if flags & bit != 0 {
        engine.key_press(key);
    } else {
        engine.key_release(key);
    }
}

/// The device-dependent flag bit for one side of a modifier pair, for the
/// modifiers this program cares about.
fn device_flag(key: Key) -> Option<u64> {
    Some(match key {
        Key::ControlLeft => 0x0000_0001,
        Key::ShiftLeft => 0x0000_0002,
        Key::ShiftRight => 0x0000_0004,
        Key::ControlRight => 0x0000_2000,
        _ => return None,
    })
}

/// Insert `text` as one event, the way a paste lands rather than the way typing
/// does.
///
/// Posting the word character by character meant one `CGEvent` per letter, each
/// needing pacing the OS wouldn't drop — tens of milliseconds of visible
/// retyping. A single keyboard event carrying the whole Unicode string arrives
/// atomically, and because it carries characters rather than key positions the
/// result is exactly the corrected word whatever layout is active.
fn paste_text(text: &str) {
    if text.is_empty() {
        return;
    }
    let utf16: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        // A press/release pair: some applications only act on one of the two,
        // and the string is attached to both so either order works.
        for down in [true, false] {
            let event = CGEventCreateKeyboardEvent(std::ptr::null_mut(), 0, down);
            if event.is_null() {
                return;
            }
            CGEventKeyboardSetUnicodeString(event, utf16.len(), utf16.as_ptr());
            CGEventPost(KCG_HID_EVENT_TAP, event);
            CFRelease(event as *const c_void);
        }
    }
}

/// Handle returned by [`setup_event_tap`]. Keep it alive for as long as the
/// keyboard listener should run; dropping it disables and releases the tap.
pub struct EventTapHandle {
    tap: CFMachPortRef,
    source: CFRunLoopSourceRef,
}

// CFMachPort / CFRunLoopSource are thread-safe Core Foundation types — fine to
// hold the raw pointer across threads.
unsafe impl Send for EventTapHandle {}
unsafe impl Sync for EventTapHandle {}

impl Drop for EventTapHandle {
    fn drop(&mut self) {
        unsafe {
            CGEventTapEnable(self.tap, false);
            CFRelease(self.source as _);
            CFRelease(self.tap as _);
        }
    }
}

/// Full macOS startup. Owns everything that used to live in `main`'s macOS
/// `cfg` block: install the keyboard event tap on the main run loop, then hand
/// the main thread to the menubar tray. Keeping it here means changes to the
/// macOS launch path can't touch the Linux or Windows paths.
pub fn start(en: Dict, he: Dict, control: Arc<AppControl>, with_gui: bool) {
    // The event tap must live on the main run loop (see `setup_event_tap`), so
    // a main-thread TUI can't coexist with it — the tray is the UI here.
    if with_gui {
        eprintln!("--gui is not supported on macOS; running with the menubar tray instead.");
    }
    // Record our PID so `recast -s` can find and terminate this instance. macOS
    // runs in the foreground (no fork/daemonize), so process::id() is the tray
    // process the user wants to stop. Without this the pidfile is never written
    // and `-s` silently no-ops.
    if let Err(e) = crate::daemon::write_pidfile() {
        eprintln!("Failed to write pidfile: {e}");
    }
    // Bind the tap to a named local so it stays alive for the whole session;
    // dropping it would disable and release the tap.
    let _tap = setup_event_tap(en, he, Arc::clone(&control));
    crate::platform::tray::run(control);
}

/// Register a system-wide keyboard tap with the main run loop. Must be called
/// from the main thread before tao's `EventLoop::run` takes it over. The tap
/// callback fires from inside NSApp's event loop, so no separate thread is
/// needed for keyboard capture (and the OS doesn't kill us for running a tap
/// on the "wrong" run loop).
pub fn setup_event_tap(
    en_dict: Dict,
    he_dict: Dict,
    control: Arc<AppControl>,
) -> Option<EventTapHandle> {
    // Silent on the way up, like Linux and Windows: the banner has already
    // greeted a terminal launch, and under the LaunchAgent this would only
    // ever land in /tmp/recast.out.log.
    let engine = Engine::<MacOs>::new(en_dict, he_dict, control, Arc::new(AtomicBool::new(false)));
    if ENGINE.set(engine).is_err() {
        eprintln!("setup_event_tap called more than once");
        return None;
    }

    unsafe {
        let tap = CGEventTapCreate(
            KCG_HID_EVENT_TAP,
            KCG_HEAD_INSERT_EVENT_TAP,
            KCG_EVENT_TAP_OPTION_LISTEN_ONLY,
            EVENT_MASK,
            tap_callback,
            std::ptr::null_mut(),
        );
        if tap.is_null() {
            eprintln!(
                "Could not create event tap. Grant 'Input Monitoring' permission \
                 in System Settings > Privacy & Security, then relaunch."
            );
            return None;
        }
        let source = CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
        if source.is_null() {
            CFRelease(tap as _);
            return None;
        }
        // Remember the tap so the callback can re-enable it if macOS disables
        // it later. Set before the tap can fire (it's enabled just below).
        let _ = TAP_PORT.set(TapPort(tap));
        CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        Some(EventTapHandle { tap, source })
    }
}
