use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use evdev::{uinput::VirtualDevice, AttributeSet, Device, EventSummary, KeyCode};

/// Maximum time `replace_word` will wait for the user to physically release
/// the keys we are about to retype before injecting anyway. Kept short: the
/// correction should feel instant, and a key still held when it expires gets a
/// synthetic release instead of more waiting (see `replace_word`).
const HELD_RELEASE_TIMEOUT: Duration = Duration::from_millis(40);

/// Longest a Ctrl press may last and still count as a *tap* rather than a hold.
/// Ctrl held down is the start of a shortcut; Ctrl let straight back up types
/// nothing and means nothing, which is what makes it usable as a gesture.
const TAP_MAX: Duration = Duration::from_millis(300);

/// Two Ctrl taps inside this window are the undo gesture. Wide enough not to
/// demand a drum roll, short enough that two unrelated taps a second apart are
/// not read as one gesture.
const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(500);

use crate::dictionary::{check_and_correct, complete_candidates, Dict, Fix};
use crate::keymap::{
    english_char_to_evkey_shifted, evkey_to_english_char, evkey_to_english_char_shifted,
    evkey_to_hebrew_char,
};
use crate::types::{AppControl, FixKind, Language, WordBuffer};

/// One key of the word being typed, with the shift state it was typed under.
/// The buffer holds key *positions*, which carry no case of their own, so the
/// shift has to be recorded here or the capitalization is lost by the time a
/// correction is typed back.
#[derive(Clone, Copy)]
pub struct Typed {
    pub key: KeyCode,
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
/// there and the payload is dropped (see `handle_key`).
pub struct LastFix {
    /// Characters our injection put on screen, terminator included — what has
    /// to come back off.
    on_screen: usize,
    /// The user's own keys, ready to be typed in its place.
    restore: Vec<(KeyCode, bool)>,
    /// Terminator to press again afterwards; `None` for a completion, which
    /// interrupted a word rather than finishing one.
    terminator: Option<KeyCode>,
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
    terminator: Option<KeyCode>,
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

/// Per-keyboard state tracked across events.
pub struct AppState {
    pub keys: WordBuffer<Typed>,
    pub last_event_time: Instant,
    pub last_keycode: Option<KeyCode>,
    pub is_replacing: bool,
    pub buffered_keys: WordBuffer<Typed>,
    /// Physical keys currently held down. Tracked from press/release events
    /// so the replace_word thread can wait for the user to lift the keys it
    /// is about to retype — otherwise the compositor squashes our synthetic
    /// press as a duplicate of the still-held physical key.
    pub held_keys: HashSet<KeyCode>,
    /// Caps Lock latch, toggled on every press. Together with the held shifts
    /// it is what decides whether a letter came out capitalized.
    pub caps_lock: bool,
    /// Right Shift went down and nothing else has been pressed since — so if it
    /// comes back up untouched, it was a tap, which is the completion request
    /// (see `handle_key`).
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
    let held = st.held_keys.contains(&KeyCode::KEY_LEFTSHIFT)
        || st.held_keys.contains(&KeyCode::KEY_RIGHTSHIFT);
    held != st.caps_lock
}

/// Full Linux startup. Owns everything that used to live in `main`'s Linux
/// `cfg` block: pick a foreground UI (control window or TUI) with the listener
/// on a background thread, or daemonize and run the listener headless. Keeping
/// it here means changes to the Linux launch path can't touch macOS or Windows.
pub fn start(
    en: Dict,
    he: Dict,
    control: Arc<AppControl>,
    with_gui: bool,
    with_window: bool,
    with_foreground: bool,
) {
    if with_window {
        // Control window: eframe owns the main thread, listener runs in the
        // background.
        let listener_control = Arc::clone(&control);
        thread::spawn(move || {
            run(en, he, listener_control);
        });
        if let Err(e) = crate::gui::run(control) {
            eprintln!("GUI error: {e}");
        }
        return;
    }
    if with_gui {
        let listener_control = Arc::clone(&control);
        thread::spawn(move || {
            run(en, he, listener_control);
        });
        if let Err(e) = crate::tui::run_tui(control) {
            eprintln!("TUI error: {e}");
        }
        return;
    }
    // Daemonize (fork, setsid, detach stdio) unless asked to stay in the
    // foreground. Under systemd (Type=simple) the service manager tracks our
    // PID, so forking would make the unit see the service as dead —
    // INVOCATION_ID is set by systemd and disables the fork automatically.
    let under_systemd = std::env::var_os("INVOCATION_ID").is_some();
    if !with_foreground && !under_systemd {
        crate::daemon::daemonize();
    }
    if let Err(e) = crate::daemon::write_pidfile() {
        eprintln!("Failed to write pidfile: {e}");
        std::process::exit(1);
    }
    run(en, he, control);
}

pub fn run(
    en_dict: Dict,
    he_dict: Dict,
    control: Arc<AppControl>,
) {
    // println!("Starting recast keyboard watcher (Linux/Wayland)...");

    // Persistent virtual device strictly for injecting backspaces and
    // corrected words.  Created once so Wayland has time to recognise it.
    let mut all_keys = AttributeSet::<KeyCode>::new();
    for code in 0u16..=249 {
        all_keys.insert(KeyCode::new(code));
    }

    let builder = match VirtualDevice::builder() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to create injector builder: {}", e);
            return;
        }
    };

    let injector = match builder.name("recast-injector").with_keys(&all_keys) {
        Ok(b) => match b.build() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Failed to build injector device: {}", e);
                return;
            }
        },
        Err(e) => {
            eprintln!("Failed to configure injector device: {}", e);
            return;
        }
    };

    // Allow time for the OS/compositor to detect the new injection device.
    thread::sleep(Duration::from_millis(300));
    let injector = Arc::new(Mutex::new(injector));

    // Find all physical keyboard and mouse devices. Mice are included so
    // that a click can reset the in-progress word buffer (parity with the
    // macOS / Windows ButtonPress handler).
    let device_paths: Vec<std::path::PathBuf> = evdev::enumerate()
        .filter_map(|(path, dev)| {
            if dev.name() == Some("recast-injector") {
                return None;
            }
            let keys = dev.supported_keys();
            let is_keyboard = keys.is_some_and(|k| k.contains(KeyCode::KEY_A));
            let is_mouse = keys.is_some_and(|k| k.contains(KeyCode::BTN_LEFT));
            if is_keyboard || is_mouse {
                Some(path)
            } else {
                None
            }
        })
        .collect();

     if device_paths.is_empty() {
         eprintln!("No input devices found. Make sure you are in the 'input' group.");
         eprintln!("Hint: Run 'sudo usermod -aG input $USER' and log out/in.");
         return;
     }

    // println!("Found {} input device(s).", device_paths.len());

    let state = Arc::new(Mutex::new(AppState {
        keys: WordBuffer::new(),
        last_event_time: Instant::now(),
        last_keycode: None,
        is_replacing: false,
        buffered_keys: WordBuffer::new(),
        held_keys: HashSet::new(),
        caps_lock: false,
        right_shift_tap: false,
        ctrl_down: None,
        last_ctrl_tap: None,
        last_action: None,
        cycle: None,
    }));

    let mut handles = vec![];

    for path in device_paths {
        let state = Arc::clone(&state);
        let injector = Arc::clone(&injector);
        let control = Arc::clone(&control);
        let path_clone = path.clone();

        let handle = thread::spawn(move || {
            let mut dev = match Device::open(&path_clone) {
                Ok(d) => d,
                 Err(e) => {
                     eprintln!("Could not open {:?}: {}", path_clone, e);
                     eprintln!("Hint: Are you in the 'input' group? Run 'sudo usermod -aG input $USER' and log out/in.");
                     return;
                 }
            };

            // println!("Passively listening on {:?}", path_clone);

            loop {
                let events = match dev.fetch_events() {
                    Ok(ev) => ev.collect::<Vec<_>>(),
                    Err(e) => {
                        eprintln!("Error reading {:?}: {}", path_clone, e);
                        thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                };

                for event in events {
                    if let EventSummary::Key(_, keycode, value) = event.destructure() {
                        match value {
                            1 => handle_key(
                                keycode, &state, en_dict, he_dict, &injector, &control,
                            ),
                            0 => handle_release(
                                keycode, &state, en_dict, he_dict, &injector, &control,
                            ),
                            _ => {}
                        }
                    }
                }
            }
        });
        handles.push(handle);
    }

    // println!("Listening for keyboard events. Press Space or Enter to check a word.");
    for h in handles {
        let _ = h.join();
    }
}

/// Process a single key-press event.
fn handle_key(
    key: KeyCode,
    state_mutex: &Arc<Mutex<AppState>>,
    en_dict: Dict,
    he_dict: Dict,
    injector: &Arc<Mutex<VirtualDevice>>,
    control: &Arc<AppControl>,
) {
    use evdev::KeyCode as KC;

    let mut st = state_mutex.lock().unwrap();

    // Deduplicate the same key-press arriving from multiple event nodes within 5 ms.
    let now = Instant::now();
    if st.last_keycode == Some(key)
        && now.duration_since(st.last_event_time) < Duration::from_millis(5)
    {
        return;
    }
    st.last_event_time = now;
    st.last_keycode = Some(key);
    st.held_keys.insert(key);
    if key == KC::KEY_CAPSLOCK {
        st.caps_lock = !st.caps_lock;
    }
    // Any key other than Right Shift itself means the shift is being *held* for
    // something, not tapped, so it is no longer a completion request.
    st.right_shift_tap = key == KC::KEY_RIGHTSHIFT;
    // Same idea for Ctrl, which is the undo gesture: a Ctrl with another key on
    // top of it is a shortcut, and only a bare press/release pair is a tap.
    let is_ctrl = key == KC::KEY_LEFTCTRL || key == KC::KEY_RIGHTCTRL;
    st.ctrl_down = is_ctrl.then_some(now);
    // Undo and the completion cycle both describe the text sitting at the
    // cursor right now. Any key that is not one of their own triggers moves the
    // text on, and both become claims about something that is no longer there.
    if !is_ctrl && key != KC::KEY_RIGHTSHIFT {
        st.last_action = None;
        st.cycle = None;
        st.last_ctrl_tap = None;
    }
    let shift = shift_active(&st);

    match key {
        KC::KEY_SPACE | KC::KEY_ENTER | KC::KEY_KPENTER => {
            if st.is_replacing {
                st.buffered_keys.push(Typed { key, shift });
                return;
            }

            if !st.keys.is_empty() {
                if !control.is_enabled() {
                    st.keys.clear();
                    return;
                }
                let result = check_and_correct(
                    &st.keys,
                    |t| evkey_to_english_char_shifted(t.key, t.shift),
                    |t| evkey_to_hebrew_char(t.key),
                    |t| t.shift,
                    en_dict,
                    he_dict,
                );

                // Describe the fix for the history before `replacement`
                // consumes it.
                let note = result.as_ref().map(|fix| note_of(&st.keys, fix));
                if let Some(rep) = replacement(&st.keys, result) {
                    if let Some((from, to, kind)) = &note {
                        control.record_fix(from, to, *kind);
                    }
                    st.is_replacing = true;
                    let undo = undo_of(&st.keys, &rep, Some(key));
                    // +1 for the terminator the user physically typed, which is
                    // erased along with the word and pressed again afterwards.
                    let erase = rep.erase + 1;
                    let injector_clone = Arc::clone(injector);
                    let state_clone = Arc::clone(state_mutex);
                    thread::spawn(move || {
                        replace_word(
                            erase,
                            rep.retype,
                            Some(key),
                            Vec::new(),
                            Some(undo),
                            &injector_clone,
                            &state_clone,
                        );
                    });
                } else if let Some(word) = crate::dictionary::declined_by_list(
                    &st.keys,
                    |t: Typed| evkey_to_english_char_shifted(t.key, t.shift),
                    |t: Typed| evkey_to_hebrew_char(t.key),
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
        KC::KEY_BACKSPACE => {
            if st.is_replacing {
                st.buffered_keys.pop();
            } else {
                st.keys.pop();
            }
        }
        // Cursor / focus-shifting keys and mouse clicks end the current word
        // without checking it, so a stale buffer doesn't leak into the next word.
        KC::KEY_TAB
        | KC::KEY_ESC
        | KC::KEY_LEFT
        | KC::KEY_RIGHT
        | KC::KEY_UP
        | KC::KEY_DOWN
        | KC::KEY_HOME
        | KC::KEY_END
        | KC::KEY_PAGEUP
        | KC::KEY_PAGEDOWN
        | KC::KEY_INSERT
        | KC::KEY_DELETE
        | KC::BTN_LEFT
        | KC::BTN_RIGHT
        | KC::BTN_MIDDLE => {
            if st.is_replacing {
                st.buffered_keys.clear();
            } else {
                st.keys.clear();
            }
        }
        _ => {
            if evkey_to_english_char(key).is_some() || evkey_to_hebrew_char(key).is_some() {
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
/// the user is still holding (so `replace_word` can avoid injecting a press the
/// compositor would swallow as a duplicate), spotting the Right Shift *tap*
/// that asks for a completion, and spotting the Ctrl double-tap that takes a
/// correction back.
///
/// Both gestures are built on modifier taps for the same reason: Ctrl and Right
/// Shift are the only keys on every keyboard that type nothing and mean nothing
/// to the focused application on their own, so a tap of either can't move
/// focus, indent a line or open the editor's own completion popup the way Tab
/// would — nothing has to be un-done when ReCast declines. Holding either one
/// (for a capital, for a shortcut) is unaffected; only a press and release with
/// nothing in between counts.
fn handle_release(
    key: KeyCode,
    state_mutex: &Arc<Mutex<AppState>>,
    en_dict: Dict,
    he_dict: Dict,
    injector: &Arc<Mutex<VirtualDevice>>,
    control: &Arc<AppControl>,
) {
    use evdev::KeyCode as KC;

    let mut st = state_mutex.lock().unwrap();
    st.held_keys.remove(&key);

    if key == KC::KEY_LEFTCTRL || key == KC::KEY_RIGHTCTRL {
        handle_ctrl_tap(st, state_mutex, en_dict, he_dict, injector, control);
        return;
    }

    if key != KC::KEY_RIGHTSHIFT || !std::mem::take(&mut st.right_shift_tap) {
        return;
    }
    if st.is_replacing || !control.is_enabled() {
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
                |t: Typed| evkey_to_english_char_shifted(t.key, t.shift),
                |t: Typed| t.shift,
                en_dict,
            );
            if candidates.is_empty() {
                return;
            }
            (st.keys.to_vec(), candidates, 0, st.keys.len())
        }
    };

    // Past the end of the list is the user's own text, replayed from the keys
    // they pressed rather than rebuilt from a string — the odd irreproducible
    // capitalisation (`sHiFtY`) survives that way.
    let back_to_typed = index >= candidates.len();
    let retype: Vec<(KeyCode, bool)> = if back_to_typed {
        typed.iter().map(|t| (t.key, t.shift)).collect()
    } else {
        // An untypeable candidate is dropped rather than injected in half; the
        // cycle carries on to the next tap.
        match typeable(&candidates[index]) {
            Some(keys) => keys,
            None => return,
        }
    };

    // The counter tracks words changed, not taps: cycling from one guess to the
    // next is still the one fix, and landing back on what the user typed is
    // none at all.
    if back_to_typed {
        control.record_undo();
    } else if index == 0 {
        control.record_fix(
            &reading(&typed, Language::English),
            &candidates[index],
            FixKind::Complete,
        );
    }

    // The buffer has to end up holding what is on screen, or the next Space
    // would check a word the user is no longer looking at. An expansion may
    // carry spaces, in which case only the last word of it is still in progress.
    let keep: Vec<Typed> = retype
        .rsplit(|(k, _)| *k == KC::KEY_SPACE)
        .next()
        .unwrap_or_default()
        .iter()
        .map(|&(key, shift)| Typed { key, shift })
        .collect();
    // A completion can be taken back with the undo gesture too — except when it
    // has just handed back the user's own text, which is nothing to undo.
    let undo = (!back_to_typed).then(|| LastFix {
        on_screen: retype.len(),
        restore: typed.iter().map(|t| (t.key, t.shift)).collect(),
        terminator: None,
        layout: None,
        keep: typed.clone(),
        suppress: non_empty(reading(&typed, Language::English)),
    });

    st.is_replacing = true;
    st.cycle = Some(Cycle {
        typed,
        candidates,
        index,
        on_screen: retype.len(),
    });
    let injector_clone = Arc::clone(injector);
    let state_clone = Arc::clone(state_mutex);
    drop(st);
    thread::spawn(move || {
        // The completion key types nothing, so only what is on screen for the
        // partial word is erased and there is no terminator to press again.
        replace_word(erase, retype, None, keep, undo, &injector_clone, &state_clone);
    });
}

/// A Ctrl key came back up. If it was a bare tap and the second one inside
/// [`DOUBLE_TAP_WINDOW`], act on the word the cursor is sitting on — take back
/// the correction that landed on it, or take it off the user's list and correct
/// it after all. Which of the two is decided by what happened to the word, not
/// by the gesture: see [`LastAction`].
///
/// Either way it erases backwards from the cursor, so it is only ever offered
/// for a word nothing has been typed over yet (`AppState::last_action`, cleared
/// by the next keystroke). That is the same bargain macOS and iOS make, and it
/// is what keeps a mistimed double-tap from eating text further back.
fn handle_ctrl_tap(
    mut st: std::sync::MutexGuard<'_, AppState>,
    state_mutex: &Arc<Mutex<AppState>>,
    en_dict: Dict,
    he_dict: Dict,
    injector: &Arc<Mutex<VirtualDevice>>,
    control: &Arc<AppControl>,
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
        Some(LastAction::Fixed(fix)) => undo_fix(st, fix, state_mutex, injector, control),
        Some(LastAction::Skipped(skip)) => {
            unlist_and_correct(st, skip, state_mutex, en_dict, he_dict, injector, control)
        }
        None => {}
    }
}

/// Put back what the user typed before the correction on screen replaced it.
fn undo_fix(
    mut st: std::sync::MutexGuard<'_, AppState>,
    fix: LastFix,
    state_mutex: &Arc<Mutex<AppState>>,
    injector: &Arc<Mutex<VirtualDevice>>,
    control: &Arc<AppControl>,
) {
    // Put the layout back before the keys go out — `uinput` speaks keycodes, so
    // what they spell depends on the layout that is live when they land. If the
    // OS refuses the switch, replaying them would just re-enter the correction:
    // leave the text alone rather than churn it, and leave the word correctable
    // rather than retire it on the strength of an undo that never happened.
    if let Some(lang) = fix.layout {
        if !crate::layout::switch_layout_to(lang) {
            return;
        }
    }
    // Retire the word before putting it back: a correction is a function of
    // what was typed, so without this the very next repetition would be
    // corrected again and undo would be a treadmill.
    if let Some(word) = &fix.suppress {
        crate::complete::suppress(word);
    }
    control.record_undo();
    st.is_replacing = true;
    st.cycle = None;
    let injector_clone = Arc::clone(injector);
    let state_clone = Arc::clone(state_mutex);
    drop(st);
    thread::spawn(move || {
        replace_word(
            fix.on_screen,
            fix.restore,
            fix.terminator,
            fix.keep,
            // Undoing an undo would be a redo, which is a different gesture.
            None,
            &injector_clone,
            &state_clone,
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
    en_dict: Dict,
    he_dict: Dict,
    injector: &Arc<Mutex<VirtualDevice>>,
    control: &Arc<AppControl>,
) {
    crate::complete::unlist(&skip.word);
    let result = check_and_correct(
        &skip.keys,
        |t: Typed| evkey_to_english_char_shifted(t.key, t.shift),
        |t: Typed| evkey_to_hebrew_char(t.key),
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
    let injector_clone = Arc::clone(injector);
    let state_clone = Arc::clone(state_mutex);
    drop(st);
    thread::spawn(move || {
        replace_word(
            erase,
            rep.retype,
            skip.terminator,
            Vec::new(),
            None,
            &injector_clone,
            &state_clone,
        );
    });
}

/// The text a key sequence spells under `lang` — what was on screen before a
/// correction rewrote it.
fn reading(keys: &[Typed], lang: Language) -> String {
    keys.iter()
        .filter_map(|t| match lang {
            Language::English => evkey_to_english_char_shifted(t.key, t.shift),
            Language::Hebrew => evkey_to_hebrew_char(t.key),
        })
        .collect()
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
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

/// Spell `text` out as key presses, or `None` if any character can't be typed
/// under the English layout — better to drop the fix than inject half a word.
fn typeable(text: &str) -> Option<Vec<(KeyCode, bool)>> {
    text.chars().map(english_char_to_evkey_shifted).collect()
}

/// What a [`Fix`] turns into for the replace thread.
struct Replacement {
    /// How many of the characters the user typed have to be erased.
    erase: usize,
    /// The keys (with their shift state) to inject in their place.
    retype: Vec<(KeyCode, bool)>,
    /// The layout that was live before the fix, when the fix changed it — what
    /// undo has to switch back to.
    previous_layout: Option<Language>,
}

/// Turn a [`Fix`] into what the replace thread needs: how many typed characters
/// have to be erased, and the keys (with their shift state) to inject in their
/// place.
fn replacement(keys: &[Typed], fix: Option<Fix>) -> Option<Replacement> {
    match fix? {
        // Same keys, new layout — they now produce the other language. Anything
        // before `start` is a previously-typed word that the user concatenated
        // by forgetting a space, and we want to leave it intact.
        // `text` is unused here: a `uinput` device speaks keycodes, not
        // characters, so there is no way to insert the finished word directly
        // the way macOS/Windows do. Replaying the keys is equivalent — the
        // layout has already changed, so they now produce that same text — and
        // the whole sequence goes out as one batch below, so it still lands in
        // a single frame rather than as visible retyping.
        //
        // The shift state is replayed with them only when the target is
        // English: that is what puts the capital back on a mistyped `Shalom`.
        // Hebrew has no capitals, and shift there types punctuation, so a
        // Hebrew target is replayed unshifted whatever the user held.
        Fix::Layout { start, lang, .. } => {
            let keep_shift = lang == Language::English;
            let word: Vec<(KeyCode, bool)> = keys[start..]
                .iter()
                .map(|t| (t.key, t.shift && keep_shift))
                .collect();
            Some(Replacement {
                erase: word.len(),
                retype: word,
                previous_layout: Some(lang.other()),
            })
        }
        // Same layout, different letters: erase the whole word and type the
        // corrected spelling instead. If any character turns out not to be
        // typeable, drop the fix rather than inject half a word.
        Fix::Spelling { text } => Some(Replacement {
            erase: keys.len(),
            retype: typeable(&text)?,
            previous_layout: None,
        }),
    }
}

/// Everything needed to take `rep` back again, built from the keys it is about
/// to replace.
///
/// The user's own keys are replayed rather than a saved string being retyped,
/// so an undo reproduces exactly what was there — capitals included. Under a
/// Hebrew original the shifts are dropped, for the same reason the layout
/// pipeline drops them: Hebrew has no case and shift there types punctuation.
fn undo_of(keys: &[Typed], rep: &Replacement, terminator: Option<KeyCode>) -> LastFix {
    let original = &keys[keys.len() - rep.erase..];
    let was = rep.previous_layout.unwrap_or(Language::English);
    let keep_shift = was == Language::English;
    LastFix {
        // Every key we inject produces exactly one character, plus the
        // terminator that rides along after it.
        on_screen: rep.retype.len() + usize::from(terminator.is_some()),
        restore: original
            .iter()
            .map(|t| (t.key, t.shift && keep_shift))
            .collect(),
        terminator,
        layout: rep.previous_layout,
        // The terminator has already finished this word, so nothing carries
        // over into the buffer.
        keep: Vec::new(),
        suppress: non_empty(reading(original, was)),
    }
}

/// Erase `erase` characters the user typed and inject a replacement: the same
/// keys after a layout switch, different keys for a spelling fix, or the rest of
/// the word for a completion.
///
/// `terminator` is the key that ended the word (space/enter) and gets pressed
/// again after the replacement. It is `None` for a completion, which is
/// triggered by a key that types nothing and therefore has nothing to restore.
///
/// `keep` is the word buffer to leave behind — what is now on screen for the
/// word still in progress, so a completion the user keeps typing over is
/// checked as the word they can see rather than as the tail they added.
/// `undo` is the payload the Ctrl double-tap would put back, kept only if the
/// user typed nothing while this was landing.
#[allow(clippy::too_many_arguments)]
fn replace_word(
    erase: usize,
    retype: Vec<(KeyCode, bool)>,
    terminator: Option<KeyCode>,
    keep: Vec<Typed>,
    undo: Option<LastFix>,
    injector: &Arc<Mutex<VirtualDevice>>,
    state_mutex: &Arc<Mutex<AppState>>,
) {
    use evdev::{EventType, InputEvent, KeyCode as KC, SynchronizationCode};

    let syn = || {
        InputEvent::new(
            EventType::SYNCHRONIZATION.0,
            SynchronizationCode::SYN_REPORT.0,
            0,
        )
    };

    // 1a. Only the keys we are about to *press* matter here: a synthetic press
    //     of a key the physical keyboard is still holding looks like a
    //     duplicate to the compositor and gets dropped — which is how the
    //     trailing space (and occasionally the last word letter) went missing.
    //     The erased keys are not in that set; they only cost backspaces.
    //
    //     The user is usually still holding the space that triggered this, so
    //     waiting it out is the single biggest source of delay before the
    //     correction appears. Wait only briefly, then inject a synthetic
    //     *release* for whatever is still down so the press that follows is no
    //     longer a duplicate, and go ahead anyway.
    let mut keys_of_interest: HashSet<KeyCode> = retype.iter().map(|(k, _)| *k).collect();
    keys_of_interest.extend(terminator);
    //     Both shifts are always in the set. We inject each key with exactly the
    //     shift state we decided on — none at all for a Hebrew target, where
    //     shift types punctuation rather than a capital — so a shift the *user*
    //     happens to still be holding has to come up first, and one we press
    //     ourselves would be a duplicate of it.
    keys_of_interest.insert(KC::KEY_LEFTSHIFT);
    keys_of_interest.insert(KC::KEY_RIGHTSHIFT);
    let wait_start = Instant::now();
    let still_held: Vec<KeyCode> = loop {
        let held: Vec<KeyCode> = {
            let st = state_mutex.lock().unwrap();
            keys_of_interest
                .iter()
                .copied()
                .filter(|k| st.held_keys.contains(k))
                .collect()
        };
        if held.is_empty() || wait_start.elapsed() >= HELD_RELEASE_TIMEOUT {
            break held;
        }
        thread::sleep(Duration::from_micros(100));
    };

    // 1b. Wait for the hyprctl layout switch to take effect in the compositor.
    //     hyprctl returns synchronously, so this is only the compositor's
    //     internal absorption gap — 8 ms matches the macOS TIS-settle and
    //     is enough in practice on Hyprland.
    // reduced pause, usually unnecessary

    // Clone buffered keys while holding the lock, then release it before injecting
    // any keys. The injected keystrokes re-enter handle_key which also needs the
    // state lock, so holding it here would cause a deadlock that silently drops
    // the injected space/terminator.
    let buffered = {
        let st = state_mutex.lock().unwrap();
        st.buffered_keys.to_vec()
    };

    // Build the whole erase+retype sequence as one event batch and emit it in
    // a single locked write. Each key is press / SYN / release / SYN so the
    // compositor still sees distinct keystroke frames, but there are no
    // inter-key sleeps and the injector lock is taken once instead of twice
    // per event — the dominant cost of retyping.
    let delete_count = erase + buffered.len();
    let total_keys = delete_count + retype.len() * 3 + 1 + buffered.len();
    let mut evs: Vec<InputEvent> = Vec::with_capacity(total_keys * 4);
    // 1c. Release anything the user is still holding (see 1a). A release for a
    //     key this device never pressed is harmless — the compositor just sees
    //     the key go up early, and the user's own release later is a no-op.
    for kc in &still_held {
        evs.push(InputEvent::new(EventType::KEY.0, kc.0, 0));
        evs.push(syn());
    }

    // A capital is Left Shift held around the letter — the only way to type one
    // through a device that speaks key positions.
    let mut push_char = |kc: KC, shift: bool| {
        if shift {
            evs.push(InputEvent::new(EventType::KEY.0, KC::KEY_LEFTSHIFT.0, 1));
            evs.push(syn());
        }
        evs.push(InputEvent::new(EventType::KEY.0, kc.0, 1));
        evs.push(syn());
        evs.push(InputEvent::new(EventType::KEY.0, kc.0, 0));
        evs.push(syn());
        if shift {
            evs.push(InputEvent::new(EventType::KEY.0, KC::KEY_LEFTSHIFT.0, 0));
            evs.push(syn());
        }
    };

    // 2. Erase the word + buffered keys.
    for _ in 0..delete_count {
        push_char(KC::KEY_BACKSPACE, false);
    }
    // 3. Type the corrected word.
    for (key, shift) in &retype {
        push_char(*key, *shift);
    }
    // 4. Retype the terminator (space/enter) — a completion has none.
    if let Some(terminator) = terminator {
        push_char(terminator, false);
    }
    // 5. Retype buffered keys.
    for t in &buffered {
        push_char(t.key, t.shift);
    }

    if let Ok(mut dev) = injector.lock() {
        let _ = dev.emit(&evs);
    }

    // Re-acquire the lock only to clean up state.
    let mut st = state_mutex.lock().unwrap();
    st.keys.replace_with(keep);
    st.keys.extend(buffered.iter().copied());
    // Undo erases backwards from the cursor, so it is only valid while the
    // cursor is still sitting on what we just injected. Keys the user got in
    // during the replacement were replayed after it and have moved it on.
    st.last_action = if buffered.is_empty() {
        undo.map(LastAction::Fixed)
    } else {
        None
    };
    st.buffered_keys.clear();
    st.is_replacing = false;
    // Reset the dedup guard so the injected terminator (space/enter) is not
    // silently dropped because it shares the same keycode as the physical
    // keypress that triggered this replacement (both arrive within 5 ms).
    st.last_keycode = None;
}
