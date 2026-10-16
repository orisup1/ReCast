use super::engine::{self, Engine, Plan, Platform};
use super::textkeys;
use crate::dictionary::Dict;
use crate::types::{AppControl, Language};
use rdev::Key;
use std::collections::HashMap;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
type Typed = engine::Typed<Key>;

pub struct Mac;
impl Platform for Mac {
    type Key = Key;
    type Retype = String;
    type Injector = AtomicBool;
    type Focus = Focus;
    const REQUIRES_FOCUS: bool = true;
    const QUEUE_UNDO_DURING_REPLACEMENT: bool = true;
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
    fn injection_finished() -> bool {
        // Reserved before deletion starts, so releasing input cannot fail to allocate.
        let input = crate::types::lock_forgiving(deferred_input());
        let invalidates = unsafe { !deferred_gestures_only(&input.input.events) };
        if let Some(marker) = &input.finish {
            unsafe { CGEventPost(KCG_HID_EVENT_TAP, marker.event) };
        }
        invalidates
    }
    fn input_allowed() -> bool {
        !secure_input_active()
    }
    fn is_own_focus(focus: &Focus) -> bool {
        let mut pid = 0;
        unsafe { AXUIElementGetPid(focus.0, &mut pid) == 0 && pid == std::process::id() as i32 }
    }
    fn focus() -> Option<Focus> {
        focused_target()
    }
    fn app_id(focus: &Focus) -> Option<String> {
        use cocoa::base::{id, nil};
        use cocoa::foundation::NSAutoreleasePool;
        use objc::{class, msg_send, sel, sel_impl};
        unsafe {
            let mut pid = 0;
            if AXUIElementGetPid(focus.0, &mut pid) != 0 {
                return None;
            }
            let pool = NSAutoreleasePool::new(nil);
            let app: id = msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
            let bundle: id = if app == nil {
                nil
            } else {
                msg_send![app, bundleIdentifier]
            };
            let result = if bundle == nil {
                None
            } else {
                let text: *const std::os::raw::c_char = msg_send![bundle, UTF8String];
                if text.is_null() {
                    None
                } else {
                    std::ffi::CStr::from_ptr(text)
                        .to_str()
                        .ok()
                        .map(str::to_string)
                }
            };
            let _: () = msg_send![pool, drain];
            result
        }
    }
    fn selection(focus: &Focus) -> Option<engine::Selection> {
        selected_text(focus)
    }
    fn replace_selection(
        engine: &Engine<Self>,
        focus: &Focus,
        expected: &engine::Selection,
        text: &str,
        generation: u64,
    ) -> Option<engine::Selection> {
        replace_selected_text(engine, focus, expected, text, generation)
    }
    fn input_empty(_: &AtomicBool) -> bool {
        use core_foundation::{base::TCFType, string::CFString};
        use core_foundation_sys::{base::CFGetTypeID, string::*};
        let Some(focus) = focused_target() else {
            return false;
        };
        unsafe {
            AXUIElementSetMessagingTimeout(focus.0, 0.05);
            let attribute = CFString::new("AXValue");
            let mut value = std::ptr::null();
            if AXUIElementCopyAttributeValue(focus.0, attribute.as_concrete_TypeRef(), &mut value)
                != 0
                || value.is_null()
            {
                return false;
            }
            let empty =
                CFGetTypeID(value) == CFStringGetTypeID() && CFStringGetLength(value.cast()) == 0;
            CFRelease(value);
            empty
        }
    }
    fn inject(engine: &Engine<Self>, plan: Plan<Self>, generation: u64) -> Option<Vec<Typed>> {
        inject(engine, plan, generation)
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
const KCG_SESSION_EVENT_TAP: u32 = 1;
const KCG_HEAD_INSERT_EVENT_TAP: u32 = 0;
// Active taps use Accessibility authorization. Listen-only taps use Input
// Monitoring. Physical events are deferred briefly while a replacement lands.
const KCG_EVENT_TAP_OPTION_DEFAULT: u32 = 0;

const KCG_EVENT_LEFT_MOUSE_DOWN: u32 = 1;
const KCG_EVENT_LEFT_MOUSE_UP: u32 = 2;
const KCG_EVENT_RIGHT_MOUSE_DOWN: u32 = 3;
const KCG_EVENT_RIGHT_MOUSE_UP: u32 = 4;
const KCG_EVENT_LEFT_MOUSE_DRAGGED: u32 = 6;
const KCG_EVENT_RIGHT_MOUSE_DRAGGED: u32 = 7;
const KCG_EVENT_KEY_DOWN: u32 = 10;
const KCG_EVENT_KEY_UP: u32 = 11;
/// Modifier keys — Shift, Ctrl, Caps Lock — are *not* delivered as key-down and
/// key-up on macOS. They arrive only as this event type, which is why it has to
/// be in the mask: without it the word buffer never learns that a shift was
/// held, and neither of the tap gestures (Right Shift to complete, Ctrl twice
/// to undo) can fire at all.
const KCG_EVENT_FLAGS_CHANGED: u32 = 12;
const KCG_EVENT_OTHER_MOUSE_DOWN: u32 = 25;
const KCG_EVENT_OTHER_MOUSE_UP: u32 = 26;
const KCG_EVENT_OTHER_MOUSE_DRAGGED: u32 = 27;

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
    | (1u64 << KCG_EVENT_LEFT_MOUSE_UP)
    | (1u64 << KCG_EVENT_RIGHT_MOUSE_DOWN)
    | (1u64 << KCG_EVENT_RIGHT_MOUSE_UP)
    | (1u64 << KCG_EVENT_LEFT_MOUSE_DRAGGED)
    | (1u64 << KCG_EVENT_RIGHT_MOUSE_DRAGGED)
    | (1u64 << KCG_EVENT_KEY_DOWN)
    | (1u64 << KCG_EVENT_KEY_UP)
    | (1u64 << KCG_EVENT_FLAGS_CHANGED)
    | (1u64 << KCG_EVENT_OTHER_MOUSE_DOWN)
    | (1u64 << KCG_EVENT_OTHER_MOUSE_UP)
    | (1u64 << KCG_EVENT_OTHER_MOUSE_DRAGGED);

const KCG_KEYBOARD_EVENT_KEYCODE: u32 = 9;
const EVENT_USER_DATA: u32 = 42;
const RECAST_EVENT: i64 = 0x5245_4341_5354;
const FINISH_EVENT: i64 = 0x5245_0000_0000_0000;
const FINISH_MASK: i64 = !0xffff_ffff;

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
    fn CGEventGetTimestamp(event: CGEventRef) -> u64;
    #[cfg(test)]
    fn CGEventGetType(event: CGEventRef) -> u32;
    fn CGEventCreateCopy(event: CGEventRef) -> CGEventRef;
    fn CGEventTapPostEvent(proxy: CGEventTapProxy, event: CGEventRef);
    /// Which modifiers are down *after* the event. A `flagsChanged` event says
    /// which key changed but not in which direction, so this is what turns it
    /// back into a press or a release.
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);

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
/// dropped, nothing is checked, nothing is corrected. macOS already withholds
/// the characters, but "we couldn't have read it anyway" is a weaker promise
/// than not being in the loop at all — and the
/// visible half matters too, since a correction firing inside a password field
/// would rewrite a password on the strength of a dictionary lookup.
///
/// Cheap enough to ask per keystroke: it reads a process-wide flag the window
/// server keeps, with no round trip.
fn secure_input_active() -> bool {
    unsafe { IsSecureEventInputEnabled() != 0 }
}

/// Read-only permission check for the explicitly started live log.
pub fn accessibility_granted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
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
        114 => Key::Insert,
        115 => Key::Home,
        116 => Key::PageUp,
        117 => Key::Delete,
        118 => Key::F4,
        119 => Key::End,
        120 => Key::F2,
        121 => Key::PageDown,
        122 => Key::F1,
        123 => Key::LeftArrow,
        124 => Key::RightArrow,
        125 => Key::DownArrow,
        126 => Key::UpArrow,
        other => Key::Unknown(other as u32),
    }
}

static CTX: OnceLock<Arc<Engine<Mac>>> = OnceLock::new();

#[test]
fn navigation_keycodes_reset_text() {
    for code in [114, 115, 116, 117, 119, 121, 123, 124, 125, 126] {
        assert!(textkeys::is_reset(key_from_code(code)), "keycode {code}");
    }
}

/// Raw handle to our tap, stored so the callback can re-enable it if the OS
/// disables it. `CFMachPortRef` is a thread-safe Core Foundation type.
struct TapPort(CFMachPortRef);
unsafe impl Send for TapPort {}
unsafe impl Sync for TapPort {}
static TAP_PORT: OnceLock<TapPort> = OnceLock::new();

struct DeferredEvent {
    kind: u32,
    event: CGEventRef,
}
// Copies are owned here and read only on the main thread; workers only post markers.
unsafe impl Send for DeferredEvent {}
impl Drop for DeferredEvent {
    fn drop(&mut self) {
        unsafe { CFRelease(self.event.cast()) };
    }
}

struct DeferredInput<T> {
    ticket: u32,
    active: bool,
    events: Vec<T>,
}
impl<T> Default for DeferredInput<T> {
    fn default() -> Self {
        Self {
            ticket: 0,
            active: false,
            events: Vec::new(),
        }
    }
}
impl<T> DeferredInput<T> {
    fn next_ticket(&self) -> u32 {
        self.ticket.wrapping_add(1).max(1)
    }
    fn begin(&mut self) -> u32 {
        self.ticket = self.next_ticket();
        self.active = true;
        self.ticket
    }
    fn finish(&mut self, ticket: u32) -> Option<Vec<T>> {
        if !self.active || ticket != self.ticket {
            return None;
        }
        self.active = false;
        Some(std::mem::take(&mut self.events))
    }
}
#[derive(Default)]
struct NativeDeferredInput {
    input: DeferredInput<DeferredEvent>,
    finish: Option<DeferredEvent>,
}
fn deferred_input() -> &'static Mutex<NativeDeferredInput> {
    static INPUT: OnceLock<Mutex<NativeDeferredInput>> = OnceLock::new();
    INPUT.get_or_init(|| Mutex::new(NativeDeferredInput::default()))
}

fn begin_injection(engine: &Engine<Mac>, generation: u64) -> Option<()> {
    // Serialize validation and gating with capture; never dispatch with the state locked.
    dispatch::Queue::main().exec_sync(|| {
        if !engine.replacement_valid(generation) {
            return None;
        }
        let mut input = crate::types::lock_forgiving(deferred_input());
        let tag = FINISH_EVENT | i64::from(input.input.next_ticket());
        let event = keyboard_event_with_tag(0, false, false, tag)?;
        input.input.begin();
        input.finish = Some(DeferredEvent { kind: 0, event });
        engine.injector.store(true, Ordering::Relaxed);
        Some(())
    })
}

fn gesture_input(kind: u32, key: Key, flags: u64) -> bool {
    match kind {
        KCG_EVENT_FLAGS_CHANGED => Mac::is_modifier(key),
        KCG_EVENT_KEY_UP => true,
        KCG_EVENT_KEY_DOWN => key == Key::Space && flags & 0x001d_0000 == 0,
        _ => false,
    }
}

unsafe fn deferred_gestures_only(events: &[DeferredEvent]) -> bool {
    queued_gestures(events.iter().map(|event| {
        let key = key_from_code(
            CGEventGetIntegerValueField(event.event, KCG_KEYBOARD_EVENT_KEYCODE) as u16,
        );
        (event.kind, key, CGEventGetFlags(event.event))
    }))
}

fn queued_gestures(events: impl Iterator<Item = (u32, Key, u64)>) -> bool {
    let mut held = HashMap::new();
    for (kind, key, flags) in events {
        if !gesture_input(kind, key, flags) {
            return false;
        }
        if kind == KCG_EVENT_FLAGS_CHANGED {
            if let Some(bit) = device_flag(key) {
                if flags & bit != 0 {
                    held.insert(key, ());
                } else {
                    held.remove(&key);
                }
            }
        }
    }
    // An unfinished hold must not acquire a fresh tap timestamp at replay time.
    held.is_empty()
}

fn within_gesture_window(pressed: u64, released: u64, ceiling: std::time::Duration) -> bool {
    released >= pressed && u128::from(released - pressed) <= ceiling.as_nanos()
}

unsafe fn replay_deferred(
    ctx: &Arc<Engine<Mac>>,
    proxy: CGEventTapProxy,
    events: Vec<DeferredEvent>,
) {
    let gestures_only = deferred_gestures_only(&events);
    let mut modifier_down = HashMap::new();
    let mut action_released = None;
    if !gestures_only {
        ctx.forget_everything();
    }
    for event in events {
        if gestures_only {
            if event.kind == KCG_EVENT_FLAGS_CHANGED {
                let key = key_from_code(CGEventGetIntegerValueField(
                    event.event,
                    KCG_KEYBOARD_EVENT_KEYCODE,
                ) as u16);
                let timestamp = CGEventGetTimestamp(event.event);
                if let Some(bit) = device_flag(key) {
                    if CGEventGetFlags(event.event) & bit != 0 {
                        modifier_down.insert(key, timestamp);
                    } else {
                        let mut state = crate::types::lock_forgiving(&ctx.state);
                        let action = state.action_down.is_some();
                        if modifier_down.remove(&key).is_some_and(|pressed| {
                            !within_gesture_window(pressed, timestamp, engine::TAP_MAX)
                        }) {
                            state.action_down = None;
                            state.completion_down = None;
                        }
                        if action {
                            if action_released.is_some_and(|previous| {
                                !within_gesture_window(
                                    previous,
                                    timestamp,
                                    engine::DOUBLE_TAP_WINDOW,
                                )
                            }) {
                                state.last_action_tap = None;
                            }
                            action_released = Some(timestamp);
                        }
                    }
                }
            }
            capture_event(ctx, event.kind, event.event);
        } else {
            let key = key_from_code(CGEventGetIntegerValueField(
                event.event,
                KCG_KEYBOARD_EVENT_KEYCODE,
            ) as u16);
            let mut state = crate::types::lock_forgiving(&ctx.state);
            match event.kind {
                KCG_EVENT_KEY_DOWN => {
                    state.held_keys.insert(key);
                }
                KCG_EVENT_KEY_UP => {
                    state.held_keys.remove(&key);
                }
                KCG_EVENT_FLAGS_CHANGED if key == Key::CapsLock => {
                    state.caps_lock = CGEventGetFlags(event.event) & FLAG_ALPHA_SHIFT != 0;
                    crate::layout::invalidate();
                }
                KCG_EVENT_FLAGS_CHANGED => {
                    if let Some(bit) = device_flag(key) {
                        if CGEventGetFlags(event.event) & bit != 0 {
                            state.held_keys.insert(key);
                        } else {
                            state.held_keys.remove(&key);
                        }
                        crate::layout::invalidate();
                    }
                }
                _ => {}
            }
            state.completion_down = None;
            state.action_down = None;
        }
        // Posts downstream before the next physical event; copies keep flags and releases.
        CGEventTapPostEvent(proxy, event.event);
    }
}

unsafe extern "C" fn tap_callback(
    proxy: CGEventTapProxy,
    event_type: u32,
    cg_event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    // The OS disables the tap on a callback timeout or certain input events.
    // Once disabled it delivers nothing further, so re-enable it immediately —
    // otherwise the app silently stops seeing the keyboard while the tray keeps
    // running. Handled before anything else, independent of CTX / injecting.
    if event_type == KCG_EVENT_TAP_DISABLED_BY_TIMEOUT
        || event_type == KCG_EVENT_TAP_DISABLED_BY_USER_INPUT
    {
        if let Some(port) = TAP_PORT.get() {
            CGEventTapEnable(port.0, true);
        }
        if let Some(ctx) = CTX.get() {
            ctx.forget_everything();
            if !ctx.injector.load(Ordering::Relaxed) {
                // A finish posted while the tap was disabled may have been missed.
                Mac::injection_finished();
            }
        }
        return cg_event;
    }

    let ctx = match CTX.get() {
        Some(c) => c,
        None => return cg_event,
    };

    let marker = CGEventGetIntegerValueField(cg_event, EVENT_USER_DATA);
    if marker & FINISH_MASK == FINISH_EVENT {
        let events = {
            let mut input = crate::types::lock_forgiving(deferred_input());
            let events = input.input.finish(marker as u32);
            if events.is_some() {
                input.finish.take();
            }
            events
        };
        if let Some(events) = events {
            replay_deferred(ctx, proxy, events);
        }
        // Even stale markers must never reach the application as keyboard input.
        return std::ptr::null_mut();
    }
    if marker == RECAST_EVENT {
        return cg_event;
    }

    // Secure input must never be copied into the deferred queue.
    if secure_input_active() {
        ctx.forget_everything();
        return cg_event;
    }

    {
        let mut input = crate::types::lock_forgiving(deferred_input());
        if input.input.active {
            let mut event = CGEventCreateCopy(cg_event);
            if event.is_null() {
                // Suppressed originals have no downstream writer; retaining is a lossless fallback.
                event = core_foundation_sys::base::CFRetain(cg_event.cast()).cast_mut();
            }
            input.input.events.push(DeferredEvent {
                kind: event_type,
                event,
            });
            return std::ptr::null_mut();
        }
    }

    capture_event(ctx, event_type, cg_event);
    cg_event
}

unsafe fn capture_event(ctx: &Arc<Engine<Mac>>, event_type: u32, cg_event: CGEventRef) {
    match event_type {
        KCG_EVENT_KEY_DOWN => {
            ctx.caps_lock_changed(CGEventGetFlags(cg_event) & FLAG_ALPHA_SHIFT != 0);
            let code = CGEventGetIntegerValueField(cg_event, KCG_KEYBOARD_EVENT_KEYCODE) as u16;
            ctx.key_press(key_from_code(code));
        }
        KCG_EVENT_KEY_UP => {
            let code = CGEventGetIntegerValueField(cg_event, KCG_KEYBOARD_EVENT_KEYCODE) as u16;
            ctx.key_release(key_from_code(code));
        }
        KCG_EVENT_FLAGS_CHANGED => {
            let code = CGEventGetIntegerValueField(cg_event, KCG_KEYBOARD_EVENT_KEYCODE) as u16;
            handle_flags_changed(ctx, key_from_code(code), CGEventGetFlags(cg_event));
        }
        KCG_EVENT_LEFT_MOUSE_DOWN
        | KCG_EVENT_RIGHT_MOUSE_DOWN
        | KCG_EVENT_OTHER_MOUSE_DOWN
        | KCG_EVENT_LEFT_MOUSE_DRAGGED
        | KCG_EVENT_RIGHT_MOUSE_DRAGGED
        | KCG_EVENT_OTHER_MOUSE_DRAGGED => {
            ctx.mouse_click();
        }
        _ => {}
    }
}

/// A modifier changed state. macOS never delivers these as key-down / key-up
/// (see [`KCG_EVENT_FLAGS_CHANGED`]), and the event says which key changed but
/// not in which direction — the flags say which modifiers are down afterwards,
/// so the direction is read back off them.
///
/// The documented `kCGEventFlagMask*` constants say "a shift is down" without
/// saying which one, so the *device-dependent* bits are what distinguish left
/// from right — and this whole feature is built on telling them apart.
fn handle_flags_changed(ctx: &Arc<Engine<Mac>>, key: Key, flags: u64) {
    if key == Key::CapsLock {
        // A latch rather than a held key: the flag is the state itself.
        ctx.caps_lock_changed(flags & FLAG_ALPHA_SHIFT != 0);
        crate::layout::invalidate();
        return;
    }
    let Some(bit) = device_flag(key) else {
        return;
    };
    if flags & bit != 0 {
        ctx.key_press(key);
    } else {
        ctx.key_release(key);
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
        Key::Alt => 0x0000_0020,
        Key::AltGr => 0x0000_0040,
        Key::MetaLeft => 0x0000_0008,
        Key::MetaRight => 0x0000_0010,
        _ => return None,
    })
}

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
    if !setup_guidance() {
        return;
    }
    super::start_background_tasks(&control);
    // The event tap must live on the main run loop (see `setup_event_tap`), so
    // a main-thread TUI can't coexist with it — the tray is the UI here.
    if with_gui {
        eprintln!("--gui is not supported on macOS; running with the menubar tray instead.");
    }
    if let Err(error) = crate::daemon::write_pidfile() {
        eprintln!("Failed to write pidfile: {error}");
    }
    // Bind the tap to a named local so it stays alive for the whole session;
    // dropping it would disable and release the tap.
    let Some(_tap) = setup_event_tap(en, he, Arc::clone(&control)) else {
        crate::notify::dialog("ReCast could not start", "Keyboard capture could not start. If you just granted Accessibility access, quit and reopen ReCast. If an older ReCast entry is already enabled, remove that entry and add this copy again in Privacy & Security → Accessibility.", &["Quit"]);
        std::process::exit(1);
    };
    crate::platform::tray::run(control);
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum SetupAction {
    Start,
    Check,
    Accessibility,
    Keyboards,
    Reveal,
    Quit,
}

impl SetupAction {
    fn label(self) -> &'static str {
        match self {
            Self::Start => "Start ReCast",
            Self::Check => "Check again",
            Self::Accessibility => "Open Accessibility",
            Self::Keyboards => "Open Keyboard Settings",
            Self::Reveal => "Show ReCast in Finder",
            Self::Quit => "Quit",
        }
    }

    fn settings_url(self, modern: bool) -> Option<&'static str> {
        match (self, modern) {
            (Self::Accessibility, true) => Some("x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility"),
            (Self::Accessibility, false) => Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"),
            (Self::Keyboards, true) => Some("x-apple.systempreferences:com.apple.Keyboard-Settings.extension?inputSources"),
            (Self::Keyboards, false) => Some("x-apple.systempreferences:com.apple.preference.keyboard?InputSources"),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum SetupStep {
    Accessibility,
    Keyboards,
    Ready,
}

impl SetupStep {
    fn current(accessibility: bool, english: bool, hebrew: bool) -> Self {
        if !accessibility {
            Self::Accessibility
        } else if !english || !hebrew {
            Self::Keyboards
        } else {
            Self::Ready
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Accessibility => "Set up ReCast: Accessibility",
            Self::Keyboards => "Set up ReCast: Keyboards",
            Self::Ready => "ReCast is ready",
        }
    }

    fn actions(self, can_reveal: bool) -> Vec<SetupAction> {
        match self {
            Self::Accessibility => {
                let mut actions = vec![SetupAction::Accessibility, SetupAction::Check];
                if can_reveal {
                    actions.push(SetupAction::Reveal);
                }
                actions.push(SetupAction::Quit);
                actions
            }
            Self::Keyboards => vec![
                SetupAction::Keyboards,
                SetupAction::Check,
                SetupAction::Quit,
            ],
            Self::Ready => vec![SetupAction::Start, SetupAction::Quit],
        }
    }
}

fn application_bundle(executable: &std::path::Path) -> Option<&std::path::Path> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then_some(bundle)
}

fn request_accessibility() -> bool {
    use core_foundation::{
        base::TCFType, boolean::CFBoolean, dictionary::CFDictionary, string::CFString,
    };
    let options = CFDictionary::from_CFType_pairs(&[(
        unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) },
        CFBoolean::true_value(),
    )]);
    unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) != 0 }
}

/// Native setup stays available while the user switches to System Settings.
pub fn setup_guidance() -> bool {
    use crate::notify::DialogDestination;
    let marker = crate::complete::user_path("setup-complete");
    let mut first = marker.as_ref().is_none_or(|p| !p.exists());
    let executable = std::env::current_exe().ok();
    let bundle = executable.as_deref().and_then(application_bundle);
    let permission_target = bundle.or(executable.as_deref());
    let modern = std::path::Path::new("/System/Applications/System Settings.app").exists();
    let mut destination = None;
    loop {
        let accessibility = unsafe { AXIsProcessTrusted() != 0 };
        let (english, hebrew) = crate::layout::enabled_languages();
        // The active session tap and event posting both use Accessibility.
        let step = SetupStep::current(accessibility, english, hebrew);
        if step == SetupStep::Ready && !first {
            return true;
        }
        let instruction = match step {
            SetupStep::Accessibility => "Step 1 of 2: Enable ReCast in Privacy & Security → Accessibility. Return here and choose Check again. If macOS asks, quit and reopen ReCast.",
            SetupStep::Keyboards => match (english, hebrew) {
                (false, false) => "Step 2 of 2: Open Keyboard → Text Input → Edit and add English and Hebrew. Return here and choose Check again.",
                (false, true) => "Step 2 of 2: Open Keyboard → Text Input → Edit and add English. Return here and choose Check again.",
                _ => "Step 2 of 2: Open Keyboard → Text Input → Edit and add Hebrew. Return here and choose Check again.",
            },
            SetupStep::Ready => "Start ReCast to try correction, undo, and completion in a local practice field. Close practice to skip; reopen it from Help anytime.",
        };
        let state = |ok| if ok { "Ready" } else { "Needed" };
        let mut body = format!(
            "ReCast corrects typing locally on this Mac.\n\nAccessibility: {}\nEnglish keyboard: {}\nHebrew keyboard: {}\n\n{}",
            state(accessibility), state(english), state(hebrew), instruction
        );
        if step == SetupStep::Accessibility {
            if let Some(path) = permission_target {
                body.push_str(&format!(
                    "\n\nIf it is missing, click +, press Cmd+Shift+G, and add this copy:\n{}",
                    path.display()
                ));
            }
            if bundle.is_none() {
                body.push_str("\n\nA terminal launch may appear under the terminal's name. To grant access to ReCast.app, install the app bundle and open it from Finder.");
            }
        }
        let actions = step.actions(permission_target.is_some());
        let buttons: Vec<&str> = actions.iter().map(|action| action.label()).collect();
        let choice = crate::notify::dialog_with_destination(
            step.title(),
            &body,
            &buttons,
            destination.take(),
        );
        let action = actions.get(choice).copied().unwrap_or(SetupAction::Quit);
        match action {
            SetupAction::Start => {
                if let Some(path) = &marker {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                        let _ = std::fs::write(path, "Setup completed\n");
                    }
                }
                return true;
            }
            SetupAction::Check => {
                first = true;
            }
            SetupAction::Accessibility => {
                request_accessibility();
                destination = action.settings_url(modern).map(DialogDestination::Settings);
            }
            SetupAction::Keyboards => {
                destination = action.settings_url(modern).map(DialogDestination::Settings);
            }
            SetupAction::Reveal => {
                if let Some(path) = permission_target {
                    destination = Some(DialogDestination::Reveal(path));
                } else {
                    crate::notify::dialog(
                        "Cannot locate ReCast",
                        "Quit and reopen ReCast from Finder, then try again.",
                        &["OK"],
                    );
                }
            }
            SetupAction::Quit => return false,
        }
    }
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
    // Capture, focus checks, and injection all require Accessibility.
    if !request_accessibility() {
        eprintln!(
            "ReCast needs Accessibility access to correct words. Enable ReCast in \
             System Settings > Privacy & Security > Accessibility, then relaunch."
        );
        return None;
    }

    // Silent on the way up, like Linux and Windows: the banner has already
    // greeted a terminal launch, and under the LaunchAgent this would only
    // ever land in /tmp/recast.out.log.
    let ctx = Engine::<Mac>::new(en_dict, he_dict, control, AtomicBool::new(false));

    if CTX.set(ctx).is_err() {
        eprintln!("setup_event_tap called more than once");
        return None;
    }

    unsafe {
        let tap = CGEventTapCreate(
            KCG_SESSION_EVENT_TAP,
            KCG_HEAD_INSERT_EVENT_TAP,
            KCG_EVENT_TAP_OPTION_DEFAULT,
            EVENT_MASK,
            tap_callback,
            std::ptr::null_mut(),
        );
        if tap.is_null() {
            eprintln!(
                "Could not create event tap. Quit and reopen ReCast after granting \
                 Accessibility access. If an older entry is enabled, remove it and \
                 add this copy again in System Settings > Privacy & Security > Accessibility."
            );
            return None;
        }
        let source = CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
        if source.is_null() {
            CFRelease(tap as _);
            eprintln!(
                "Could not attach ReCast's keyboard capture to the run loop. Relaunch ReCast."
            );
            return None;
        }
        // Remember the tap so the callback can re-enable it if macOS disables
        // it later. Set before the tap can fire (it's enabled just below).
        let _ = TAP_PORT.set(TapPort(tap));
        CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        CTX.get()
            .unwrap()
            .control
            .listener_ready
            .store(true, Ordering::Relaxed);
        Some(EventTapHandle { tap, source })
    }
}

fn keyboard_event(code: u16, down: bool, shift: bool) -> Option<CGEventRef> {
    keyboard_event_with_tag(code, down, shift, RECAST_EVENT)
}

fn keyboard_event_with_tag(code: u16, down: bool, shift: bool, tag: i64) -> Option<CGEventRef> {
    unsafe {
        let event = CGEventCreateKeyboardEvent(std::ptr::null_mut(), code, down);
        if event.is_null() {
            return None;
        }
        // Quartz ignores retagging an event; assign its final tag at construction.
        CGEventSetIntegerValueField(event, EVENT_USER_DATA, tag);
        // Do not inherit a physical Ctrl/Shift pressed during the correction.
        // Inherited flags can turn a backspace into a shortcut or select text.
        CGEventSetFlags(event, if shift { 0x0002_0000 } else { 0 });
        Some(event)
    }
}

fn post_key(key: Key, down: bool, shift: bool) -> Option<()> {
    let code = (0..128).find(|&code| key_from_code(code) == key)?;
    let event = keyboard_event(code, down, shift)?;
    unsafe {
        CGEventPost(KCG_HID_EVENT_TAP, event);
        CFRelease(event as _);
    }
    Some(())
}

fn paste_text(text: &str) -> Option<()> {
    if text.is_empty() {
        return Some(());
    }
    let utf16: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        // A press/release pair: some applications only act on one of the two,
        // and the string is attached to both so either order works.
        for down in [true, false] {
            let event = keyboard_event(0, down, false)?;
            CGEventKeyboardSetUnicodeString(event, utf16.len(), utf16.as_ptr());
            CGEventPost(KCG_HID_EVENT_TAP, event);
            CFRelease(event as *const c_void);
        }
    }
    Some(())
}

fn inject(engine: &Engine<Mac>, plan: Plan<Mac>, generation: u64) -> Option<Vec<Typed>> {
    let Plan {
        erase,
        retype: text,
        terminator,
    } = plan;
    let gaps = crate::timing::injection();
    if terminator == Some(Key::Return) {
        return None;
    }
    if !engine.replacement_valid(generation) {
        return None;
    }
    begin_injection(engine, generation)?;
    let buf = engine.buffered();
    // Press + release a single key with pacing that macOS won't drop. Only the
    // backspaces and the odd replayed key go through this now; the word itself
    // is one event.
    let tap_key = |k: Key, shift: bool| {
        post_key(k, true, shift)?;
        crate::timing::pause(gaps.press_gap);
        post_key(k, false, shift)?;
        crate::timing::pause(gaps.inter_key_gap);
        Some(())
    };

    let delete_count = erase + buf.len();
    for _ in 0..delete_count {
        if !engine.replacement_valid(generation) {
            return None;
        }
        tap_key(Key::Backspace, false)?;
    }
    if !engine.replacement_valid(generation) {
        return None;
    }
    match terminator {
        // The trailing space is part of the same paste, so nothing has to be
        // pressed at all.
        Some(_) => {
            paste_text(&format!("{text} "))?;
        }
        // Explicit offers already include any tracked trailing spaces.
        None => {
            paste_text(&text)?;
        }
    }
    // Keys the user managed to type while we were replacing: replayed as keys
    // (they are physical key positions, not text) once the word is back, with
    // the shift the user held so a capital stays a capital.
    for t in buf.iter() {
        if !engine.replacement_valid(generation) {
            return None;
        }
        tap_key(t.key, t.shift)?;
    }

    // The last injected key already paid `inter_key_gap`, and settling is the
    // same kind of wait for the same events — so only the difference is owed.
    crate::timing::pause(gaps.settle.saturating_sub(gaps.inter_key_gap));
    Some(buf)
}

/// An owned accessibility element; identity is compared with CFEqual, never by
/// pointer address (two queries can return different references to one field).
pub struct Focus(*const c_void);
// AXUIElement is an immutable Core Foundation reference. Only attribute reads
// are performed, and ownership stays with this reference until Drop.
unsafe impl Send for Focus {}
unsafe impl Sync for Focus {}
impl PartialEq for Focus {
    fn eq(&self, other: &Self) -> bool {
        unsafe { core_foundation_sys::base::CFEqual(self.0, other.0) != 0 }
    }
}
impl Drop for Focus {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                CFRelease(self.0);
            }
        }
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    static kAXTrustedCheckOptionPrompt: core_foundation_sys::string::CFStringRef;
    fn AXIsProcessTrustedWithOptions(
        options: core_foundation_sys::dictionary::CFDictionaryRef,
    ) -> u8;
    fn AXUIElementCreateSystemWide() -> *const c_void;
    fn AXUIElementGetPid(element: *const c_void, pid: *mut i32) -> i32;
    fn AXUIElementCopyAttributeValue(
        element: *const c_void,
        attribute: core_foundation_sys::string::CFStringRef,
        value: *mut *const c_void,
    ) -> i32;
    fn AXUIElementSetMessagingTimeout(element: *const c_void, seconds: f32) -> i32;
    fn AXUIElementIsAttributeSettable(
        element: *const c_void,
        attribute: core_foundation_sys::string::CFStringRef,
        settable: *mut u8,
    ) -> i32;
    fn AXUIElementSetAttributeValue(
        element: *const c_void,
        attribute: core_foundation_sys::string::CFStringRef,
        value: *const c_void,
    ) -> i32;
    fn AXValueGetTypeID() -> core_foundation_sys::base::CFTypeID;
    fn AXValueGetValue(value: *const c_void, kind: u32, output: *mut c_void) -> u8;
    fn AXValueCreate(kind: u32, value: *const c_void) -> *const c_void;

}

/// Keep AX ownership local. Never fetch the full document or the clipboard.
fn ax_attribute(focus: &Focus, name: &str) -> Option<core_foundation::base::CFType> {
    use core_foundation::{
        base::{CFType, TCFType},
        string::CFString,
    };
    let name = CFString::new(name);
    let mut value = std::ptr::null();
    unsafe {
        AXUIElementSetMessagingTimeout(focus.0, 0.05);
        (AXUIElementCopyAttributeValue(focus.0, name.as_concrete_TypeRef(), &mut value) == 0
            && !value.is_null())
        .then(|| CFType::wrap_under_create_rule(value))
    }
}

fn selected_text(focus: &Focus) -> Option<engine::Selection> {
    use core_foundation::{base::TCFType, string::CFString};
    use core_foundation_sys::base::{CFGetTypeID, CFRange};
    let value = ax_attribute(focus, "AXSelectedText")?;
    let string = value.downcast::<CFString>()?;
    if string.char_len() > engine::MAX_SELECTION_BYTES as isize {
        return None;
    }
    let text = string.to_string();
    if text.is_empty() || text.len() > engine::MAX_SELECTION_BYTES {
        return None;
    }
    let value = ax_attribute(focus, "AXSelectedTextRange")?;
    let mut range = CFRange {
        location: 0,
        length: 0,
    };
    unsafe {
        if CFGetTypeID(value.as_CFTypeRef()) != AXValueGetTypeID()
            || AXValueGetValue(value.as_CFTypeRef(), 4, (&mut range as *mut CFRange).cast()) == 0
        {
            return None;
        }
    }
    if range.location < 0
        || range.length <= 0
        || range.length as usize != text.encode_utf16().count()
    {
        return None;
    }
    Some(engine::Selection {
        start: range.location,
        length: range.length,
        text,
    })
}

fn replace_selected_text(
    engine: &Engine<Mac>,
    focus: &Focus,
    expected: &engine::Selection,
    text: &str,
    generation: u64,
) -> Option<engine::Selection> {
    use core_foundation::{
        base::{CFType, TCFType},
        string::CFString,
    };
    use core_foundation_sys::base::CFRange;
    let text_attribute = CFString::new("AXSelectedText");
    let range_attribute = CFString::new("AXSelectedTextRange");
    unsafe {
        for attribute in [&text_attribute, &range_attribute] {
            let mut writable = 0;
            if AXUIElementIsAttributeSettable(
                focus.0,
                attribute.as_concrete_TypeRef(),
                &mut writable,
            ) != 0
                || writable == 0
            {
                return None;
            }
        }
        let range = CFRange {
            location: expected.start,
            length: text.encode_utf16().count() as isize,
        };
        let value = AXValueCreate(4, (&range as *const CFRange).cast());
        if value.is_null() {
            return None;
        }
        let value = CFType::wrap_under_create_rule(value);
        let replacement = CFString::new(text);
        if Mac::selection(focus).as_ref() != Some(expected) || !engine.replacement_valid(generation)
        {
            return None;
        }
        // Write just the selection. Unsupported controls are left unchanged;
        // never fall back to replacing the entire document.
        if AXUIElementSetAttributeValue(
            focus.0,
            text_attribute.as_concrete_TypeRef(),
            replacement.as_CFTypeRef(),
        ) != 0
        {
            return None;
        }
        if !engine.replacement_valid(generation) {
            return None;
        }
        if AXUIElementSetAttributeValue(
            focus.0,
            range_attribute.as_concrete_TypeRef(),
            value.as_CFTypeRef(),
        ) != 0
        {
            return None;
        }
        let after = engine::Selection {
            start: range.location,
            length: range.length,
            text: text.to_owned(),
        };
        (Mac::selection(focus).as_ref() == Some(&after)).then_some(after)
    }
}

fn focused_target() -> Option<Focus> {
    use core_foundation::{base::TCFType, string::CFString};
    if secure_input_active() {
        return None;
    }
    unsafe {
        let system = Focus(AXUIElementCreateSystemWide());
        if system.0.is_null() {
            return None;
        }
        // A stalled target must not stall the global keyboard callback.
        AXUIElementSetMessagingTimeout(system.0, 0.05);
        let mut element = std::ptr::null();
        let attribute = CFString::new("AXFocusedUIElement");
        let result =
            AXUIElementCopyAttributeValue(system.0, attribute.as_concrete_TypeRef(), &mut element);
        if result == 0 && !element.is_null() {
            Some(Focus(element))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod setup_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn deferred_input_waits_for_the_latest_replacement_and_keeps_event_order() {
        let mut input = DeferredInput::<&str>::default();
        let first = input.begin();
        input.events.extend(["shift down", "key down", "key up"]);
        let second = input.begin();
        input.events.extend(["shift up", "mouse down", "mouse up"]);
        assert!(input.finish(first).is_none());
        assert!(input.active);
        assert_eq!(
            input.finish(second).unwrap(),
            [
                "shift down",
                "key down",
                "key up",
                "shift up",
                "mouse down",
                "mouse up"
            ]
        );
        assert!(!input.active);
        assert!(input.finish(second).is_none());
    }

    #[test]
    fn typing_during_each_erasure_waits_until_the_corrected_word_is_complete() {
        for arrival in 0..=8 {
            let mut input = DeferredInput::<char>::default();
            let ticket = input.begin();
            let mut screen = String::from("recieve ");
            for erased in 0..=8 {
                if erased == arrival {
                    input.events.extend("next".chars());
                }
                if erased < 8 {
                    screen.pop();
                }
            }
            screen.push_str("receive ");
            screen.extend(input.finish(ticket).unwrap());
            assert_eq!(screen, "receive next", "typing after {arrival} erasures");
        }
    }

    #[test]
    fn canceled_injection_releases_every_deferred_press_and_release() {
        let mut input = DeferredInput::<&str>::default();
        let ticket = input.begin();
        input
            .events
            .extend(["ctrl down", "v down", "v up", "ctrl up"]);
        assert_eq!(
            input.finish(ticket).unwrap(),
            ["ctrl down", "v down", "v up", "ctrl up"]
        );
        assert!(!input.active);
    }

    #[test]
    fn only_modifier_taps_and_plain_spaces_keep_correction_gestures() {
        for key in [Key::ControlLeft, Key::ShiftRight, Key::CapsLock] {
            assert!(gesture_input(KCG_EVENT_FLAGS_CHANGED, key, 0));
        }
        assert!(gesture_input(KCG_EVENT_KEY_DOWN, Key::Space, 0));
        assert!(gesture_input(KCG_EVENT_KEY_UP, Key::Space, 0x0002_0000));
        assert!(gesture_input(KCG_EVENT_KEY_UP, Key::KeyA, 0));
        assert!(!gesture_input(KCG_EVENT_KEY_DOWN, Key::KeyA, 0));
        for flags in [0x0001_0000, 0x0004_0000, 0x0008_0000, 0x0010_0000] {
            assert!(!gesture_input(KCG_EVENT_KEY_DOWN, Key::Space, flags));
        }
        for key in [Key::KeyA, Key::Return, Key::Backspace, Key::LeftArrow] {
            assert!(!gesture_input(KCG_EVENT_KEY_DOWN, key, 0));
        }
        assert!(!gesture_input(KCG_EVENT_LEFT_MOUSE_DOWN, Key::KeyA, 0));
        assert!(!gesture_input(KCG_EVENT_LEFT_MOUSE_UP, Key::KeyA, 0));
        assert!(!gesture_input(KCG_EVENT_LEFT_MOUSE_DRAGGED, Key::KeyA, 0));
    }

    #[test]
    fn replay_preserves_physical_hold_and_double_tap_limits() {
        assert!(within_gesture_window(10, 300_000_010, engine::TAP_MAX));
        assert!(!within_gesture_window(10, 300_000_011, engine::TAP_MAX));
        assert!(within_gesture_window(
            10,
            500_000_010,
            engine::DOUBLE_TAP_WINDOW
        ));
        assert!(!within_gesture_window(
            10,
            500_000_011,
            engine::DOUBLE_TAP_WINDOW
        ));
        assert!(!within_gesture_window(10, 9, engine::TAP_MAX));
        let down = (KCG_EVENT_FLAGS_CHANGED, Key::ControlLeft, 0x0000_0001);
        let up = (KCG_EVENT_FLAGS_CHANGED, Key::ControlLeft, 0);
        assert!(queued_gestures([down, up].into_iter()));
        assert!(queued_gestures([up].into_iter()));
        assert!(!queued_gestures([down].into_iter()));
    }

    #[test]
    #[ignore = "requires an unsandboxed macOS WindowServer; constructs events without posting"]
    fn injected_events_have_only_the_requested_modifiers() {
        // Construct events without posting them or capturing any real typing.
        for code in [0, 36, 51] {
            for down in [false, true] {
                for shift in [false, true] {
                    let event = keyboard_event(code, down, shift).unwrap();
                    unsafe {
                        assert_eq!(CGEventGetFlags(event), if shift { 0x0002_0000 } else { 0 });
                        assert_eq!(
                            CGEventGetIntegerValueField(event, EVENT_USER_DATA),
                            RECAST_EVENT
                        );
                        CFRelease(event as _);
                    }
                }
            }
        }
        for ticket in [1, u32::MAX] {
            let value = FINISH_EVENT | i64::from(ticket);
            let marker = keyboard_event_with_tag(0, false, false, value).unwrap();
            unsafe {
                assert_eq!(CGEventGetIntegerValueField(marker, EVENT_USER_DATA), value);
                assert_eq!(value & FINISH_MASK, FINISH_EVENT);
                assert_ne!(RECAST_EVENT & FINISH_MASK, FINISH_EVENT);
                assert_eq!(CGEventGetType(marker), KCG_EVENT_KEY_UP);
                CFRelease(marker.cast());
            }
        }
    }

    #[test]
    fn setup_guides_the_next_requirement_only() {
        for accessibility in [false, true] {
            for english in [false, true] {
                for hebrew in [false, true] {
                    let step = SetupStep::current(accessibility, english, hebrew);
                    let actions = step.actions(true);
                    if !accessibility {
                        assert_eq!(step, SetupStep::Accessibility);
                        assert_eq!(actions[0], SetupAction::Accessibility);
                        assert!(actions.contains(&SetupAction::Reveal));
                        assert!(!actions.contains(&SetupAction::Keyboards));
                    } else if !english || !hebrew {
                        assert_eq!(step, SetupStep::Keyboards);
                        assert_eq!(actions[0], SetupAction::Keyboards);
                        assert!(!actions.contains(&SetupAction::Accessibility));
                        assert!(!actions.contains(&SetupAction::Reveal));
                    } else {
                        assert_eq!(step, SetupStep::Ready);
                        assert_eq!(actions, [SetupAction::Start, SetupAction::Quit]);
                    }
                    assert_eq!(actions.last(), Some(&SetupAction::Quit));
                    assert_eq!(
                        actions.contains(&SetupAction::Start),
                        accessibility && english && hebrew
                    );
                }
            }
        }
        assert!(!SetupStep::Accessibility
            .actions(false)
            .contains(&SetupAction::Reveal));
    }

    #[test]
    fn permission_target_is_the_running_copy_not_another_installed_bundle() {
        for path in ["/Applications/ReCast.app", "/tmp/local build/ReCast.app"] {
            let executable = Path::new(path).join("Contents/MacOS/recast");
            assert_eq!(application_bundle(&executable), Some(Path::new(path)));
        }
        for path in [
            "/usr/local/bin/recast",
            "/tmp/ReCast.app/recast",
            "/tmp/ReCast.app/Contents/Helpers/recast",
        ] {
            assert_eq!(application_bundle(Path::new(path)), None);
        }
        let accessibility = SetupAction::Accessibility.settings_url(true).unwrap();
        let keyboards = SetupAction::Keyboards.settings_url(true).unwrap();
        assert!(accessibility.ends_with("PrivacySecurity.extension?Privacy_Accessibility"));
        assert!(keyboards.ends_with("Keyboard-Settings.extension?inputSources"));
        assert!(SetupAction::Accessibility
            .settings_url(false)
            .unwrap()
            .ends_with("security?Privacy_Accessibility"));
        assert!(SetupAction::Keyboards
            .settings_url(false)
            .unwrap()
            .ends_with("keyboard?InputSources"));
        assert!(SetupAction::Start.settings_url(true).is_none());
        assert!(SetupAction::Reveal.settings_url(true).is_none());
        assert!(SetupAction::Quit.settings_url(true).is_none());
    }
}
