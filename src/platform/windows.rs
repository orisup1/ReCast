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
    VK_RETURN, VK_SHIFT, VK_SPACE,
};

use crate::dictionary::{check_and_correct, complete_candidates, Dict, Fix};
use crate::keymap::{
    english_char_to_key, key_to_english_char, key_to_english_char_shifted, key_to_hebrew_char,
};
use crate::types::{AppControl, FixKind, Language};

/// Maximum time the replace thread will wait for the user to physically
/// release the keys we are about to retype before injecting anyway.
const HELD_RELEASE_TIMEOUT: Duration = Duration::from_millis(150);

/// Grace period between the last injected event and un-gating the listener.
/// `SendInput` returns as soon as the events are queued, so clearing the gate
/// immediately can let our own tail end up back in the word buffer.
const INJECT_SETTLE: Duration = Duration::from_millis(2);

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
/// there and the payload is dropped (see the `KeyPress` arm of the listener).
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
    pub keys: Vec<Typed>,
    pub is_replacing: bool,
    pub buffered_keys: Vec<Typed>,
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
    /// (see the `KeyRelease` arm of the listener).
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

/// Whether a letter pressed right now would come out capitalized.
fn shift_active(st: &AppState) -> bool {
    let held = st.held_keys.contains(&Key::ShiftLeft) || st.held_keys.contains(&Key::ShiftRight);
    held != st.caps_lock
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
        caps_lock: false,
        right_shift_tap: false,
        ctrl_down: None,
        last_ctrl_tap: None,
        last_action: None,
        cycle: None,
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
                if key == Key::CapsLock {
                    st.caps_lock = !st.caps_lock;
                }
                // Any key other than Right Shift itself means the shift is
                // being *held* for something, not tapped, so it is no longer a
                // completion request.
                st.right_shift_tap = key == Key::ShiftRight;
                // Same idea for Ctrl, which is the undo gesture: a Ctrl with
                // another key on top of it is a shortcut, and only a bare
                // press/release pair is a tap.
                let is_ctrl = key == Key::ControlLeft || key == Key::ControlRight;
                st.ctrl_down = is_ctrl.then(Instant::now);
                // Undo and the completion cycle both describe the text sitting
                // at the cursor right now. Any key that is not one of their own
                // triggers moves the text on, and both become claims about
                // something that is no longer there.
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
                            if !control_cb.is_enabled() {
                                st.keys.clear();
                                return;
                            }
                            let result = check_and_correct(
                                &st.keys,
                                |t: Typed| key_to_english_char_shifted(t.key, t.shift),
                                |t: Typed| key_to_hebrew_char(t.key),
                                |t: Typed| t.shift,
                                en_dict,
                                he_dict,
                            );

                            // Describe the fix for the history before
                            // `replacement` consumes it.
                            let note = result.as_ref().map(|fix| note_of(&st.keys, fix));
                            if let Some(rep) = replacement(&st.keys, result) {
                                if let Some((from, to, kind)) = &note {
                                    control_cb.record_fix(from, to, *kind);
                                }
                                st.is_replacing = true;
                                let terminator = Some(key);
                                let undo = undo_of(&st.keys, &rep, terminator);
                                // +1 for the terminator the user physically
                                // typed, erased with the word and pressed again
                                // afterwards.
                                let erase = rep.erase + 1;
                                let state_clone = Arc::clone(&state_cb);
                                let injecting_flag = Arc::clone(&injecting);

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
                                // Nothing happened to this word, and the only
                                // reason is that the user has it listed. Arm the
                                // gesture to change their mind about it.
                                let skip = LastSkip {
                                    keys: st.keys.clone(),
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
                                st.buffered_keys.push(Typed { key, shift });
                            } else {
                                st.keys.push(Typed { key, shift });
                            }
                        }
                    }
                }
            }
            // Releases matter for three things: knowing which keys the user is
            // still holding (so the replace thread can avoid injecting a press
            // Windows would swallow as a duplicate), spotting the Right Shift
            // *tap* that asks for a completion, and spotting the Ctrl
            // double-tap that takes a correction back.
            //
            // Both gestures are built on modifier taps for the same reason:
            // Ctrl and Right Shift are the only keys on every keyboard that
            // type nothing and mean nothing to the focused application on their
            // own, so a tap of either can't move focus, indent a line or open
            // the editor's own completion popup the way Tab would — nothing has
            // to be un-done when ReCast declines. Holding either one (for a
            // capital, for a shortcut) is unaffected; only a press and release
            // with nothing in between counts.
            EventType::KeyRelease(key) => {
                st.held_keys.remove(&key);
                if key == Key::ControlLeft || key == Key::ControlRight {
                    handle_ctrl_tap(
                        st, &state_cb, &injecting, &control_cb, en_dict, he_dict,
                    );
                    return;
                }
                if key != Key::ShiftRight || !std::mem::take(&mut st.right_shift_tap) {
                    return;
                }
                handle_completion_tap(st, &state_cb, &injecting, &control_cb, en_dict);
            }
            EventType::ButtonPress(_) => {
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
    };

    let _ = writeln!(std::io::stdout(), "Listening for keyboard events.");
    if let Err(err) = listen(callback) {
        let _ = writeln!(std::io::stderr(), "Error while listening for keyboard events: {err:?}");
    }
}

/// The completion key was tapped: either step to the next guess in the cycle
/// already running, or start one from the word in the buffer.
fn handle_completion_tap(
    mut st: std::sync::MutexGuard<'_, AppState>,
    state_mutex: &Arc<Mutex<AppState>>,
    injecting: &Arc<AtomicBool>,
    control: &Arc<AppControl>,
    en_dict: Dict,
) {
    if st.is_replacing || !control.is_enabled() {
        return;
    }

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
                en_dict,
            );
            if candidates.is_empty() {
                return;
            }
            (st.keys.clone(), candidates, 0, st.keys.len())
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
        control.record_undo();
    } else if index == 0 {
        control.record_fix(&reading(&typed, Language::English), &text, FixKind::Complete);
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
    let state_clone = Arc::clone(state_mutex);
    let injecting_flag = Arc::clone(injecting);
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
/// the next keystroke). That is the same bargain every in-place autocorrect
/// makes, and it is what keeps a mistimed double-tap from eating text further
/// back.
fn handle_ctrl_tap(
    mut st: std::sync::MutexGuard<'_, AppState>,
    state_mutex: &Arc<Mutex<AppState>>,
    injecting: &Arc<AtomicBool>,
    control: &Arc<AppControl>,
    en_dict: Dict,
    he_dict: Dict,
) {
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

    if st.is_replacing || !control.is_enabled() {
        return;
    }
    match st.last_action.take() {
        Some(LastAction::Fixed(fix)) => undo_fix(st, fix, state_mutex, injecting, control),
        Some(LastAction::Skipped(skip)) => {
            unlist_and_correct(st, skip, state_mutex, injecting, control, en_dict, he_dict)
        }
        None => {}
    }
}

/// Put back what the user typed before the correction on screen replaced it.
fn undo_fix(
    mut st: std::sync::MutexGuard<'_, AppState>,
    fix: LastFix,
    state_mutex: &Arc<Mutex<AppState>>,
    injecting: &Arc<AtomicBool>,
    control: &Arc<AppControl>,
) {
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
    control.record_undo();
    st.is_replacing = true;
    st.cycle = None;
    let state_clone = Arc::clone(state_mutex);
    let injecting_flag = Arc::clone(injecting);
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
#[allow(clippy::too_many_arguments)]
fn unlist_and_correct(
    mut st: std::sync::MutexGuard<'_, AppState>,
    skip: LastSkip,
    state_mutex: &Arc<Mutex<AppState>>,
    injecting: &Arc<AtomicBool>,
    control: &Arc<AppControl>,
    en_dict: Dict,
    he_dict: Dict,
) {
    crate::complete::unlist(&skip.word);
    let result = check_and_correct(
        &skip.keys,
        |t: Typed| key_to_english_char_shifted(t.key, t.shift),
        |t: Typed| key_to_hebrew_char(t.key),
        |t: Typed| t.shift,
        en_dict,
        he_dict,
    );
    let note = result.as_ref().map(|fix| note_of(&skip.keys, fix));
    // Off the list, but the pipelines have nothing to say about it after all —
    // which is a fine outcome, and not one to rewrite the screen over.
    let Some(rep) = replacement(&skip.keys, result) else {
        return;
    };

    if let Some((from, to, kind)) = &note {
        control.record_fix(from, to, *kind);
    }
    st.is_replacing = true;
    st.cycle = None;
    let erase = rep.erase + usize::from(skip.terminator.is_some());
    let state_clone = Arc::clone(state_mutex);
    let injecting_flag = Arc::clone(injecting);
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

/// Turn a [`Fix`] into what the replace thread needs: how many of the typed
/// keys have to be erased, and the finished text to put in their place.
///
/// Unlike Linux (which can only replay keycodes through `uinput`), Windows can
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
    send(&mut inputs);

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
        send(&mut replay);
    }

    thread::sleep(INJECT_SETTLE);
    let mut st = state_mutex.lock().unwrap();
    let buffered_typed = !buf.is_empty();
    st.keys = keep;
    st.keys.extend(buf);
    // Undo erases backwards from the cursor, so it is only valid while the
    // cursor is still sitting on what we just injected. Keys the user got in
    // during the replacement were replayed after it and have moved it on.
    st.last_action = if buffered_typed {
        None
    } else {
        undo.map(LastAction::Fixed)
    };
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
