use std::collections::HashSet;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use rdev::{simulate, EventType, Key};

use crate::dictionary::check_and_switch_with_split;
use crate::keymap::{key_to_english_char, key_to_hebrew_char};
use crate::types::AppControl;

/// Maximum time the replace thread will wait for the user to physically
/// release the keys we are about to retype before injecting anyway.
const HELD_RELEASE_TIMEOUT: Duration = Duration::from_millis(150);

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
type CFIndex = isize;

const KCG_HID_EVENT_TAP: u32 = 0;
const KCG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const KCG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;

const KCG_EVENT_LEFT_MOUSE_DOWN: u32 = 1;
const KCG_EVENT_RIGHT_MOUSE_DOWN: u32 = 3;
const KCG_EVENT_KEY_DOWN: u32 = 10;
const KCG_EVENT_KEY_UP: u32 = 11;
const KCG_EVENT_OTHER_MOUSE_DOWN: u32 = 25;

const EVENT_MASK: u64 = (1u64 << KCG_EVENT_LEFT_MOUSE_DOWN)
    | (1u64 << KCG_EVENT_RIGHT_MOUSE_DOWN)
    | (1u64 << KCG_EVENT_KEY_DOWN)
    | (1u64 << KCG_EVENT_KEY_UP)
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

struct TapContext {
    state: Arc<Mutex<AppState>>,
    control: Arc<AppControl>,
    en_dict: HashSet<String>,
    he_dict: HashSet<String>,
    injecting: Arc<AtomicBool>,
}

static CTX: OnceLock<TapContext> = OnceLock::new();

unsafe extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: u32,
    cg_event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    let ctx = match CTX.get() {
        Some(c) => c,
        None => return cg_event,
    };

    if ctx.injecting.load(Ordering::Relaxed) {
        return cg_event;
    }

    match event_type {
        KCG_EVENT_KEY_DOWN => {
            let code = CGEventGetIntegerValueField(cg_event, KCG_KEYBOARD_EVENT_KEYCODE) as u16;
            handle_key_press(ctx, key_from_code(code));
        }
        KCG_EVENT_KEY_UP => {
            let code = CGEventGetIntegerValueField(cg_event, KCG_KEYBOARD_EVENT_KEYCODE) as u16;
            let mut st = ctx.state.lock().unwrap();
            st.held_keys.remove(&key_from_code(code));
        }
        KCG_EVENT_LEFT_MOUSE_DOWN
        | KCG_EVENT_RIGHT_MOUSE_DOWN
        | KCG_EVENT_OTHER_MOUSE_DOWN => {
            let mut st = ctx.state.lock().unwrap();
            if st.is_replacing {
                st.buffered_keys.clear();
            } else {
                st.keys.clear();
            }
        }
        _ => {}
    }

    cg_event
}

fn handle_key_press(ctx: &TapContext, key: Key) {
    let mut st = ctx.state.lock().unwrap();
    st.held_keys.insert(key);
    match key {
        Key::Space | Key::Return => {
            if st.is_replacing {
                st.buffered_keys.push(key);
                return;
            }

            if !st.keys.is_empty() {
                if !ctx.control.is_enabled() {
                    st.keys.clear();
                    return;
                }
                let result = check_and_switch_with_split(
                    &st.keys,
                    key_to_english_char,
                    key_to_hebrew_char,
                    &ctx.en_dict,
                    &ctx.he_dict,
                );
                if let Some(start) = result {
                    ctx.control.record_fix();
                    st.is_replacing = true;
                    // See linux.rs: anything before `start` is a
                    // previously-typed word the user concatenated
                    // by forgetting a space; leave it untouched.
                    let keys_clone: Vec<Key> = st.keys[start..].to_vec();
                    let state_clone = Arc::clone(&ctx.state);
                    let terminator = key;
                    let injecting_flag = Arc::clone(&ctx.injecting);

                    thread::spawn(move || {
                        replace_word(keys_clone, terminator, &state_clone, &injecting_flag);
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
        Key::Tab | Key::Escape | Key::LeftArrow | Key::RightArrow | Key::UpArrow
        | Key::DownArrow => {
            if st.is_replacing {
                st.buffered_keys.clear();
            } else {
                st.keys.clear();
            }
        }
        _ => {
            if key_to_english_char(key).is_some() || key_to_hebrew_char(key).is_some() {
                if st.is_replacing {
                    st.buffered_keys.push(key);
                } else {
                    st.keys.push(key);
                }
            }
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

/// Register a system-wide keyboard tap with the main run loop. Must be called
/// from the main thread before tao's `EventLoop::run` takes it over. The tap
/// callback fires from inside NSApp's event loop, so no separate thread is
/// needed for keyboard capture (and the OS doesn't kill us for running a tap
/// on the "wrong" run loop).
pub fn setup_event_tap(
    en_dict: HashSet<String>,
    he_dict: HashSet<String>,
    control: Arc<AppControl>,
) -> Option<EventTapHandle> {
    println!("Starting recast keyboard watcher (macOS)...");

    let ctx = TapContext {
        state: Arc::new(Mutex::new(AppState {
            keys: Vec::new(),
            is_replacing: false,
            buffered_keys: Vec::new(),
            held_keys: HashSet::new(),
        })),
        control,
        en_dict,
        he_dict,
        injecting: Arc::new(AtomicBool::new(false)),
    };

    if CTX.set(ctx).is_err() {
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
        CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        Some(EventTapHandle { tap, source })
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

    // 2. Layout-settle window after switch_layout_to (TIS) before retype.
    thread::sleep(Duration::from_millis(2));

    // 3. Gate the listener now that we are about to inject our own events.
    injecting.store(true, Ordering::Relaxed);

    let buf = {
        let st = state_mutex.lock().unwrap();
        st.buffered_keys.clone()
    };

    let delete_count = keys.len() + 1 + buf.len();
    for _ in 0..delete_count {
        let _ = simulate(&EventType::KeyPress(Key::Backspace));
        let _ = simulate(&EventType::KeyRelease(Key::Backspace));
        thread::sleep(Duration::from_micros(50));
    }
    for k in &keys {
        let _ = simulate(&EventType::KeyPress(*k));
        let _ = simulate(&EventType::KeyRelease(*k));
        thread::sleep(Duration::from_micros(50));
    }
    let _ = simulate(&EventType::KeyPress(terminator));
    let _ = simulate(&EventType::KeyRelease(terminator));
    for k in buf.iter() {
        let _ = simulate(&EventType::KeyPress(*k));
        let _ = simulate(&EventType::KeyRelease(*k));
        thread::sleep(Duration::from_micros(50));
    }

    let mut st = state_mutex.lock().unwrap();
    st.keys = buf;
    st.buffered_keys.clear();
    st.is_replacing = false;
    injecting.store(false, Ordering::Relaxed);
}
