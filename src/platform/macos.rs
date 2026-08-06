use std::collections::HashSet;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use rdev::{simulate, EventType, Key};

use crate::dictionary::{check_and_correct, complete_candidates, Dict, Fix};
use crate::keymap::{
    english_char_to_key, key_to_english_char, key_to_english_char_shifted, key_to_hebrew_char,
};
use crate::types::{
    lock_forgiving, AppControl, FixKind, Language, Replaceable, ReplaceGuard, WordBuffer,
};

/// Longest a Ctrl press may last and still count as a *tap* rather than a hold.
/// Ctrl held down is the start of a shortcut; Ctrl let straight back up types
/// nothing and means nothing, which is what makes it usable as a gesture.
const TAP_MAX: Duration = Duration::from_millis(300);

/// Two Ctrl taps inside this window are the undo gesture. Wide enough not to
/// demand a drum roll, short enough that two unrelated taps a second apart are
/// not read as one gesture.
const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(500);

/// One key of the word being typed, with the shift state it was typed under.
/// The buffer holds key *positions*, which carry no case of their own, so the
/// shift has to be recorded here or the capitalization is lost by the time a
/// correction is typed back.
#[derive(Clone, Copy)]
pub struct Typed {
    pub key: Key,
    pub shift: bool,
}

/// What the Ctrl double-tap would do to the word the cursor is sitting on.
///
/// The gesture is a *toggle* over the user's lists, which is why both cases
/// live behind one field: a word has either just been corrected (so the gesture
/// takes the correction back and retires the word) or just been left alone
/// because it is already retired (so the gesture un-retires it and corrects it
/// after all). Never both, and never anything else — a word that is simply
/// spelled right does not arm it.
pub enum LastAction {
    /// A correction landed and the cursor is still on it.
    Fixed(LastFix),
    /// A word was passed over only because it is on one of the user's lists.
    Skipped(LastSkip),
}

/// A correction that is on screen right now, with the cursor still sitting
/// immediately after it — everything the Ctrl double-tap needs to put back what
/// the user actually typed.
///
/// It is only kept for that moment. Undo erases backwards from the cursor, so
/// once the user types anything else the correction is no longer what sits
/// there and the payload is dropped (see `handle_key_press`).
pub struct LastFix {
    /// Characters our injection put on screen, terminator included — what has
    /// to come back off.
    on_screen: usize,
    /// The text that was there before, exactly as the user typed it.
    restore: String,
    /// Terminator to press again afterwards; `None` for a completion, which
    /// interrupted a word rather than finishing one.
    terminator: Option<Key>,
    /// Layout to switch back to, when the correction was the one that changed
    /// it. Restoring the letters without restoring the layout would leave the
    /// user typing the wrong language into the word they just rescued.
    layout: Option<Language>,
    /// Word buffer to leave behind: a completion's original prefix, empty for a
    /// word the terminator already finished.
    keep: Vec<Typed>,
    /// The reading to stop correcting for the rest of the session. Undo that
    /// only rewrote the screen would be undone again by the next repetition of
    /// the same word (see `complete::suppress`).
    suppress: Option<String>,
}

/// A word the pipelines passed over because the user had already told us to
/// leave it alone — what the Ctrl double-tap needs to change its mind.
///
/// The keys are kept rather than the decision, because there is no decision
/// yet: the word was never put through the pipelines with the list out of the
/// way. The gesture takes it off the list and runs them then.
pub struct LastSkip {
    /// The word as typed, to run the pipelines over once it is off the list.
    keys: Vec<Typed>,
    /// The terminator already on screen after it, erased with the word and
    /// pressed again afterwards exactly as on the normal path.
    terminator: Option<Key>,
    /// The reading that is on the list.
    word: String,
}

/// A completion cycle: the guesses on offer for the word being typed, and which
/// one is currently on screen.
///
/// `index == candidates.len()` is the entry past the end — what the user typed
/// — so tapping through the whole list always arrives back at their own text
/// rather than stranding them on the last guess.
pub struct Cycle {
    /// The word buffer as the user typed it, before any completion.
    typed: Vec<Typed>,
    candidates: Vec<String>,
    index: usize,
    /// Characters the current offer put on screen, to erase for the next one.
    on_screen: usize,
}

pub struct AppState {
    pub keys: WordBuffer<Typed>,
    pub is_replacing: bool,
    pub buffered_keys: WordBuffer<Typed>,
    /// Physical keys currently held down. Tracked from press/release events
    /// so the replace thread can wait for the user to lift the keys it is
    /// about to retype — otherwise the OS sees the synthetic press as a
    /// duplicate of the still-held physical key and drops it.
    pub held_keys: HashSet<Key>,
    /// Caps Lock latch, toggled on every press. Together with the held shifts
    /// it is what decides whether a letter came out capitalized.
    pub caps_lock: bool,
    /// Right Shift went down and nothing else has been pressed since — so if it
    /// comes back up untouched, it was a tap, which is the completion request
    /// (see `handle_key_release`).
    pub right_shift_tap: bool,
    /// When a Ctrl key went down with nothing pressed since. `None` once
    /// another key joins it, because that makes it a shortcut rather than a
    /// tap.
    pub ctrl_down: Option<Instant>,
    /// When the last completed Ctrl tap happened; a second one inside
    /// [`DOUBLE_TAP_WINDOW`] is the undo gesture.
    pub last_ctrl_tap: Option<Instant>,
    /// What the Ctrl double-tap would do to the word the cursor is sitting on,
    /// if it would do anything.
    pub last_action: Option<LastAction>,
    /// The completion cycle in progress, if the user is tapping through guesses.
    pub cycle: Option<Cycle>,
}

impl Replaceable for AppState {
    fn set_replacing(&mut self, replacing: bool) {
        self.is_replacing = replacing;
    }
    fn clear_buffered(&mut self) {
        self.buffered_keys.clear();
    }
}

/// Whether a letter pressed right now would come out capitalized.
fn shift_active(st: &AppState) -> bool {
    let held = st.held_keys.contains(&Key::ShiftLeft) || st.held_keys.contains(&Key::ShiftRight);
    held != st.caps_lock
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

struct TapContext {
    state: Arc<Mutex<AppState>>,
    control: Arc<AppControl>,
    en_dict: Dict,
    he_dict: Dict,
    injecting: Arc<AtomicBool>,
}

static CTX: OnceLock<TapContext> = OnceLock::new();

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
    // running. Handled before anything else, independent of CTX / injecting.
    if event_type == KCG_EVENT_TAP_DISABLED_BY_TIMEOUT
        || event_type == KCG_EVENT_TAP_DISABLED_BY_USER_INPUT
    {
        if let Some(port) = TAP_PORT.get() {
            CGEventTapEnable(port.0, true);
        }
        return cg_event;
    }

    let ctx = match CTX.get() {
        Some(c) => c,
        None => return cg_event,
    };

    if ctx.injecting.load(Ordering::Relaxed) {
        return cg_event;
    }

    // A password field has focus: drop whatever is buffered and look away until
    // it doesn't. Clearing rather than merely skipping matters — the buffer may
    // hold the start of a word typed a moment before the field took focus, and
    // that half-word must not be joined to what is typed into it, nor still be
    // sitting there to be corrected when focus comes back.
    if secure_input_active() {
        if let Ok(mut st) = ctx.state.lock() {
            st.keys.clear();
            st.last_action = None;
            st.cycle = None;
        }
        return cg_event;
    }

    match event_type {
        KCG_EVENT_KEY_DOWN => {
            let code = CGEventGetIntegerValueField(cg_event, KCG_KEYBOARD_EVENT_KEYCODE) as u16;
            handle_key_press(ctx, key_from_code(code));
        }
        KCG_EVENT_KEY_UP => {
            let code = CGEventGetIntegerValueField(cg_event, KCG_KEYBOARD_EVENT_KEYCODE) as u16;
            handle_key_release(ctx, key_from_code(code));
        }
        KCG_EVENT_FLAGS_CHANGED => {
            let code = CGEventGetIntegerValueField(cg_event, KCG_KEYBOARD_EVENT_KEYCODE) as u16;
            handle_flags_changed(ctx, key_from_code(code), CGEventGetFlags(cg_event));
        }
        KCG_EVENT_LEFT_MOUSE_DOWN
        | KCG_EVENT_RIGHT_MOUSE_DOWN
        | KCG_EVENT_OTHER_MOUSE_DOWN => {
            let mut st = lock_forgiving(&ctx.state);
            if st.is_replacing {
                st.buffered_keys.clear();
            } else {
                st.keys.clear();
            }
            // A click can put the cursor anywhere, so neither gesture is
            // describing the text in front of it any more — and undo erases
            // backwards from wherever the cursor now is.
            st.last_action = None;
            st.cycle = None;
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
fn handle_flags_changed(ctx: &TapContext, key: Key, flags: u64) {
    if key == Key::CapsLock {
        // A latch rather than a held key: the flag is the state itself.
        lock_forgiving(&ctx.state).caps_lock = flags & FLAG_ALPHA_SHIFT != 0;
        return;
    }
    let Some(bit) = device_flag(key) else {
        return;
    };
    if flags & bit != 0 {
        handle_key_press(ctx, key);
    } else {
        handle_key_release(ctx, key);
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

fn handle_key_press(ctx: &TapContext, key: Key) {
    let mut st = lock_forgiving(&ctx.state);
    st.held_keys.insert(key);
    // Any key other than Right Shift itself means the shift is being *held* for
    // something, not tapped, so it is no longer a completion request.
    st.right_shift_tap = key == Key::ShiftRight;
    // Same idea for Ctrl, which is the undo gesture: a Ctrl with another key on
    // top of it is a shortcut, and only a bare press/release pair is a tap.
    let is_ctrl = key == Key::ControlLeft || key == Key::ControlRight;
    st.ctrl_down = is_ctrl.then(Instant::now);
    // Undo and the completion cycle both describe the text sitting at the
    // cursor right now. Any key that is not one of their own triggers moves the
    // text on, and both become claims about something that is no longer there.
    if !is_ctrl && key != Key::ShiftRight {
        st.last_action = None;
        st.cycle = None;
        st.last_ctrl_tap = None;
    }
    let shift = shift_active(&st);
    match key {
        Key::Space | Key::Return => {
            if st.is_replacing {
                st.buffered_keys.push(Typed { key, shift });
                return;
            }

            if !st.keys.is_empty() {
                if !ctx.control.is_enabled() {
                    st.keys.clear();
                    return;
                }
                let result = check_and_correct(
                    &st.keys,
                    |t: Typed| key_to_english_char_shifted(t.key, t.shift),
                    |t: Typed| key_to_hebrew_char(t.key),
                    |t: Typed| t.shift,
                    ctx.en_dict,
                    ctx.he_dict,
                );
                // Describe the fix for the history before `replacement`
                // consumes it.
                let note = result.as_ref().map(|fix| note_of(&st.keys, fix));
                if let Some(rep) = replacement(&st.keys, result) {
                    if let Some((from, to, kind)) = &note {
                        ctx.control.record_fix(from, to, *kind);
                    }
                    st.is_replacing = true;
                    let state_clone = Arc::clone(&ctx.state);
                    let terminator = Some(key);
                    let undo = undo_of(&st.keys, &rep, terminator);
                    // +1 for the terminator the user physically typed, which is
                    // erased with the word and pressed again afterwards.
                    let erase = rep.erase + 1;
                    let injecting_flag = Arc::clone(&ctx.injecting);

                    thread::spawn(move || {
                        replace_word(
                            erase,
                            rep.text,
                            terminator,
                            Vec::new(),
                            Some(undo),
                            &state_clone,
                            &injecting_flag,
                        );
                    });
                } else if let Some(word) = crate::dictionary::declined_by_list(
                    &st.keys,
                    |t: Typed| key_to_english_char_shifted(t.key, t.shift),
                    |t: Typed| key_to_hebrew_char(t.key),
                    |t: Typed| t.shift,
                ) {
                    // Nothing happened to this word, and the only reason is
                    // that the user has it listed. Arm the gesture to change
                    // their mind about it.
                    let skip = LastSkip {
                        keys: st.keys.to_vec(),
                        terminator: Some(key),
                        word,
                    };
                    st.last_action = Some(LastAction::Skipped(skip));
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
        // Cursor / focus-shifting keys end the current word without checking
        // it, so a stale buffer doesn't leak into the next word. Kept in sync
        // with the Windows and Linux listeners.
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
            if key_to_english_char(key).is_some() || key_to_hebrew_char(key).is_some() {
                if st.is_replacing {
                    st.buffered_keys.push(Typed { key, shift });
                } else {
                    st.keys.push(Typed { key, shift });
                }
            }
        }
    }
}

/// Process a key release. Releases matter for three things: knowing which keys
/// the user is still holding (so the replace thread can avoid injecting a press
/// the OS would swallow as a duplicate), spotting the Right Shift *tap* that
/// asks for a completion, and spotting the Ctrl double-tap that takes a
/// correction back.
///
/// Both gestures are built on modifier taps for the same reason: Ctrl and Right
/// Shift are the only keys on every keyboard that type nothing and mean nothing
/// to the focused application on their own, so a tap of either can't move
/// focus, indent a line or open the editor's own completion popup the way Tab
/// would — nothing has to be un-done when ReCast declines. Holding either one
/// (for a capital, for a shortcut) is unaffected; only a press and release with
/// nothing in between counts.
fn handle_key_release(ctx: &TapContext, key: Key) {
    let mut st = lock_forgiving(&ctx.state);
    st.held_keys.remove(&key);

    if key == Key::ControlLeft || key == Key::ControlRight {
        handle_ctrl_tap(ctx, st);
        return;
    }

    if key != Key::ShiftRight || !std::mem::take(&mut st.right_shift_tap) {
        return;
    }
    if st.is_replacing || !ctx.control.is_enabled() {
        return;
    }

    // Either step to the next guess in the cycle already running, or start one
    // from the word in the buffer.
    let (typed, candidates, index, erase) = match st.cycle.take() {
        Some(cycle) => {
            let next = if cycle.index >= cycle.candidates.len() {
                0
            } else {
                cycle.index + 1
            };
            (cycle.typed, cycle.candidates, next, cycle.on_screen)
        }
        None => {
            if st.keys.is_empty() {
                return;
            }
            let candidates = complete_candidates(
                &st.keys,
                |t: Typed| key_to_english_char_shifted(t.key, t.shift),
                |t: Typed| t.shift,
                ctx.en_dict,
            );
            if candidates.is_empty() {
                return;
            }
            (st.keys.to_vec(), candidates, 0, st.keys.len())
        }
    };

    // Past the end of the list is the user's own text, rebuilt from the keys
    // they pressed rather than from a candidate — the odd irreproducible
    // capitalisation (`sHiFtY`) survives that way.
    let back_to_typed = index >= candidates.len();
    let text = if back_to_typed {
        reading(&typed, Language::English)
    } else {
        candidates[index].clone()
    };

    // The counter tracks words changed, not taps: cycling from one guess to the
    // next is still the one fix, and landing back on what the user typed is
    // none at all.
    if back_to_typed {
        ctx.control.record_undo();
    } else if index == 0 {
        ctx.control
            .record_fix(&reading(&typed, Language::English), &text, FixKind::Complete);
    }

    // The buffer has to end up holding what is on screen, or the next Space
    // would check a word the user is no longer looking at.
    let keep = if back_to_typed {
        typed.clone()
    } else {
        buffer_of(&text)
    };
    // A completion can be taken back with the undo gesture too — except when it
    // has just handed back the user's own text, which is nothing to undo.
    let restore = reading(&typed, Language::English);
    let undo = (!back_to_typed).then(|| LastFix {
        on_screen: text.chars().count(),
        suppress: non_empty(restore.clone()),
        restore,
        terminator: None,
        layout: None,
        keep: typed.clone(),
    });

    st.is_replacing = true;
    st.cycle = Some(Cycle {
        typed,
        candidates,
        index,
        on_screen: text.chars().count(),
    });
    let state_clone = Arc::clone(&ctx.state);
    let injecting_flag = Arc::clone(&ctx.injecting);
    drop(st);
    thread::spawn(move || {
        // The completion key types nothing, so only what is on screen for the
        // partial word is erased and there is no terminator to press again.
        replace_word(erase, text, None, keep, undo, &state_clone, &injecting_flag);
    });
}

/// A Ctrl key came back up. If it was a bare tap and the second one inside
/// [`DOUBLE_TAP_WINDOW`], take back the correction the cursor is sitting on.
///
/// Undo erases backwards from the cursor, so it is only ever offered for a
/// correction nothing has been typed over yet (`AppState::last_action`, cleared by
/// the next keystroke). That is the same bargain macOS makes for its own
/// autocorrect, and it is what keeps a mistimed double-tap from eating text
/// further back.
fn handle_ctrl_tap(ctx: &TapContext, mut st: std::sync::MutexGuard<'_, AppState>) {
    let Some(down) = st.ctrl_down.take() else {
        return;
    };
    // Held rather than tapped: the user was using Ctrl for what it is for.
    if down.elapsed() > TAP_MAX {
        st.last_ctrl_tap = None;
        return;
    }
    let now = Instant::now();
    match st.last_ctrl_tap.take() {
        Some(prev) if now.duration_since(prev) <= DOUBLE_TAP_WINDOW => {}
        // First tap of a possible pair: remember it and wait for the second.
        _ => {
            st.last_ctrl_tap = Some(now);
            return;
        }
    }

    if st.is_replacing || !ctx.control.is_enabled() {
        return;
    }
    match st.last_action.take() {
        Some(LastAction::Fixed(fix)) => undo_fix(ctx, st, fix),
        Some(LastAction::Skipped(skip)) => unlist_and_correct(ctx, st, skip),
        None => {}
    }
}

/// Put back what the user typed before the correction on screen replaced it.
fn undo_fix(ctx: &TapContext, mut st: std::sync::MutexGuard<'_, AppState>, fix: LastFix) {
    // Retire the word before putting it back: a correction is a function of
    // what was typed, so without this the very next repetition would be
    // corrected again and undo would be a treadmill.
    if let Some(word) = &fix.suppress {
        crate::complete::suppress(word);
    }
    // The restored text is layout-independent, but what the user types *next*
    // is not: put the layout back the way they had it.
    if let Some(lang) = fix.layout {
        crate::layout::switch_layout_to(lang);
    }
    ctx.control.record_undo();
    st.is_replacing = true;
    st.cycle = None;
    let state_clone = Arc::clone(&ctx.state);
    let injecting_flag = Arc::clone(&ctx.injecting);
    drop(st);
    thread::spawn(move || {
        replace_word(
            fix.on_screen,
            fix.restore,
            fix.terminator,
            fix.keep,
            // Undoing an undo would be a redo, which is a different gesture.
            None,
            &state_clone,
            &injecting_flag,
        );
    });
}

/// Take the word off the user's lists and run the pipelines over it again —
/// the other half of the toggle, for a word that was passed over *because* it
/// was listed.
///
/// The correction is applied exactly as it would have been a moment ago: the
/// terminator on screen is erased with the word and pressed again after it. No
/// new undo is armed, because taking this one back would put the word straight
/// onto the list the gesture just took it off.
fn unlist_and_correct(ctx: &TapContext, mut st: std::sync::MutexGuard<'_, AppState>, skip: LastSkip) {
    crate::complete::unlist(&skip.word);
    let result = check_and_correct(
        &skip.keys,
        |t: Typed| key_to_english_char_shifted(t.key, t.shift),
        |t: Typed| key_to_hebrew_char(t.key),
        |t: Typed| t.shift,
        ctx.en_dict,
        ctx.he_dict,
    );
    let note = result.as_ref().map(|fix| note_of(&skip.keys, fix));
    // Off the list, but the pipelines have nothing to say about it after all —
    // which is a fine outcome, and not one to rewrite the screen over.
    let Some(rep) = replacement(&skip.keys, result) else {
        return;
    };

    if let Some((from, to, kind)) = &note {
        ctx.control.record_fix(from, to, *kind);
    }
    st.is_replacing = true;
    st.cycle = None;
    let erase = rep.erase + usize::from(skip.terminator.is_some());
    let state_clone = Arc::clone(&ctx.state);
    let injecting_flag = Arc::clone(&ctx.injecting);
    drop(st);
    thread::spawn(move || {
        replace_word(
            erase,
            rep.text,
            skip.terminator,
            Vec::new(),
            None,
            &state_clone,
            &injecting_flag,
        );
    });
}

/// The text a key sequence spells under `lang`, capitals included — what was on
/// screen before a correction rewrote it.
fn reading(keys: &[Typed], lang: Language) -> String {
    keys.iter()
        .filter_map(|t| match lang {
            Language::English => key_to_english_char_shifted(t.key, t.shift)
                .map(|c| if t.shift { c.to_ascii_uppercase() } else { c }),
            // Hebrew has no case, so the shift the user held says nothing.
            Language::Hebrew => key_to_hebrew_char(t.key),
        })
        .collect()
}

/// The pair of words the recent-corrections history shows for `fix`, and which
/// pipeline produced it.
///
/// The "before" side is what was on screen, which is not the same reading in
/// both cases: a layout fix has already switched to `lang`, so what the user
/// was looking at is the *other* layout's reading, while a spelling fix or an
/// expansion never left English.
fn note_of(keys: &[Typed], fix: &Fix) -> (String, String, FixKind) {
    match fix {
        Fix::Layout { start, text, lang } => (
            reading(&keys[*start..], lang.other()),
            text.clone(),
            FixKind::Layout,
        ),
        Fix::Spelling { text } => (
            reading(keys, Language::English),
            text.clone(),
            FixKind::Spelling,
        ),
    }
}

/// The word buffer that matches `text` now being on screen. Only the last word
/// of it is still in progress — an abbreviation expansion may carry spaces —
/// and anything the English layout can't type is dropped rather than guessed at.
fn buffer_of(text: &str) -> Vec<Typed> {
    text.rsplit(' ')
        .next()
        .unwrap_or_default()
        .chars()
        .filter_map(|c| english_char_to_key(c).map(|(key, shift)| Typed { key, shift }))
        .collect()
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
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
pub fn start(
    en: Dict,
    he: Dict,
    control: Arc<AppControl>,
    with_gui: bool,
) {
    // The event tap must live on the main run loop (see `setup_event_tap`), so
    // a main-thread TUI can't coexist with it — the tray is the UI here.
    if with_gui {
        eprintln!("--gui is not supported on macOS; running with the menubar tray instead.");
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
    println!("Starting recast keyboard watcher (macOS)...");

    let ctx = TapContext {
        state: Arc::new(Mutex::new(AppState {
            keys: WordBuffer::new(),
            is_replacing: false,
            buffered_keys: WordBuffer::new(),
            held_keys: HashSet::new(),
            caps_lock: false,
            right_shift_tap: false,
            ctrl_down: None,
            last_ctrl_tap: None,
            last_action: None,
            cycle: None,
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
        // Remember the tap so the callback can re-enable it if macOS disables
        // it later. Set before the tap can fire (it's enabled just below).
        let _ = TAP_PORT.set(TapPort(tap));
        CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        Some(EventTapHandle { tap, source })
    }
}

/// Turn a [`Fix`] into what the replace thread needs: how many of the typed
/// keys have to be erased, and the finished text to put in their place.
///
/// Unlike Linux (which can only replay keycodes through `uinput`), macOS can
/// insert the characters themselves, so both kinds of fix reduce to the same
/// thing — erase N, insert a string — and neither depends on the keys being
/// typeable under the active layout.
fn replacement(keys: &[Typed], fix: Option<Fix>) -> Option<Replacement> {
    match fix? {
        // Anything before `start` is a previously-typed word the user
        // concatenated by forgetting a space; leave it intact. `text` already
        // carries the capitalization the user typed — inserting characters
        // rather than key positions means there is no shift to replay.
        Fix::Layout { start, text, lang } => Some(Replacement {
            erase: keys.len() - start,
            text,
            previous_layout: Some(lang.other()),
        }),
        Fix::Spelling { text } => Some(Replacement {
            erase: keys.len(),
            text,
            previous_layout: None,
        }),
    }
}

/// What a [`Fix`] turns into for the replace thread.
struct Replacement {
    /// How many of the characters the user typed have to be erased.
    erase: usize,
    /// The finished text to put in their place.
    text: String,
    /// The layout that was live before the fix, when the fix changed it — what
    /// undo has to switch back to.
    previous_layout: Option<Language>,
}

/// Everything needed to take `rep` back again, built from the keys it is about
/// to replace.
fn undo_of(keys: &[Typed], rep: &Replacement, terminator: Option<Key>) -> LastFix {
    let original = &keys[keys.len() - rep.erase..];
    let was = rep.previous_layout.unwrap_or(Language::English);
    let restore = reading(original, was);
    LastFix {
        // Everything we insert is one character per key, plus the terminator
        // that rides along after it (inside the text for a space, pressed for a
        // Return).
        on_screen: rep.text.chars().count() + usize::from(terminator.is_some()),
        suppress: non_empty(restore.clone()),
        restore,
        terminator,
        layout: rep.previous_layout,
        // The terminator has already finished this word, so nothing carries
        // over into the buffer.
        keep: Vec::new(),
    }
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

/// Erase the word the user typed and put the corrected one in its place.
///
/// `erase` is the number of characters to delete, the terminator the user typed
/// included. `terminator` is the key to press again afterwards, or `None` for a
/// completion — which is asked for with a key that types nothing and so has
/// nothing to restore.
///
/// `keep` is the word buffer to leave behind — what is now on screen for the
/// word still in progress, so a completion the user keeps typing over is
/// checked as the word they can see rather than as the tail they added.
/// `undo` is the payload the Ctrl double-tap would put back, kept only if the
/// user typed nothing while this was landing.
fn replace_word(
    erase: usize,
    text: String,
    terminator: Option<Key>,
    keep: Vec<Typed>,
    undo: Option<LastFix>,
    state_mutex: &Arc<Mutex<AppState>>,
    injecting: &Arc<AtomicBool>,
) {
    let gaps = crate::timing::injection();
    // Armed for the whole replacement: whatever happens below — including a
    // panic — `is_replacing` and the `injecting` gate are cleared. Leaving
    // `injecting` set is the worse of the two failures: the tap keeps running
    // and every key the user types is discarded as though it were ours.
    let _gate = ReplaceGuard::new(state_mutex.as_ref(), Some(injecting.as_ref()));

    // 1. The corrected word goes in as text, so the only real key we press is
    //    Return (a space rides along inside the text instead). Wait for the
    //    user to lift it first: a synthetic press of a still-held key is
    //    swallowed as a duplicate. injecting=false here so release events from
    //    the listener still update held_keys.
    let needs_return = terminator == Some(Key::Return);
    if needs_return {
        let wait_start = Instant::now();
        loop {
            let still_held = {
                let st = lock_forgiving(state_mutex);
                st.held_keys.contains(&Key::Return)
            };
            if !still_held || wait_start.elapsed() >= gaps.held_release_timeout {
                break;
            }
            crate::timing::pause(gaps.held_poll);
        }
    }

    // 2. switch_layout_to already polled until the new layout took effect, and
    //    the pasted text is layout-independent anyway.

    // 3. Gate the listener now that we are about to inject our own events.
    injecting.store(true, Ordering::Relaxed);

    let buf = {
        let st = lock_forgiving(state_mutex);
        st.buffered_keys.to_vec()
    };

    // Press + release a single key with pacing that macOS won't drop. Only the
    // backspaces and the odd replayed key go through this now; the word itself
    // is one event.
    let tap_key = |k: Key| {
        let _ = simulate(&EventType::KeyPress(k));
        crate::timing::pause(gaps.press_gap);
        let _ = simulate(&EventType::KeyRelease(k));
        crate::timing::pause(gaps.inter_key_gap);
    };

    let delete_count = erase + buf.len();
    for _ in 0..delete_count {
        tap_key(Key::Backspace);
    }
    match terminator {
        Some(Key::Return) => {
            paste_text(&text);
            tap_key(Key::Return);
        }
        // The trailing space is part of the same paste, so nothing has to be
        // pressed at all.
        Some(_) => paste_text(&format!("{text} ")),
        // A completion ends mid-word: no terminator, no trailing space.
        None => paste_text(&text),
    }
    // Keys the user managed to type while we were replacing: replayed as keys
    // (they are physical key positions, not text) once the word is back, with
    // the shift the user held so a capital stays a capital.
    for t in buf.iter() {
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

    // The last injected key already paid `inter_key_gap`, and settling is the
    // same kind of wait for the same events — so only the difference is owed.
    crate::timing::pause(gaps.settle.saturating_sub(gaps.inter_key_gap));
    let mut st = lock_forgiving(state_mutex);
    let buffered_typed = !buf.is_empty();
    st.keys.replace_with(keep);
    st.keys.extend(buf);
    // Undo erases backwards from the cursor, so it is only valid while the
    // cursor is still sitting on what we just injected. Keys the user got in
    // during the replacement were replayed after it and have moved it on.
    st.last_action = if buffered_typed {
        None
    } else {
        undo.map(LastAction::Fixed)
    };
    // `buffered_keys`, `is_replacing` and the `injecting` gate are the guard's,
    // cleared after this lock is dropped — on this path and a panicking one
    // alike.
}
