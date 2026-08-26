use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use evdev::{uinput::VirtualDevice, AttributeSet, Device, EventSummary, KeyCode};

/// Longest a Ctrl press may last and still count as a *tap* rather than a hold.
/// Ctrl held down is the start of a shortcut; Ctrl let straight back up types
/// nothing and means nothing, which is what makes it usable as a gesture.
const TAP_MAX: Duration = Duration::from_millis(300);

/// Two Ctrl taps inside this window are the undo gesture. Wide enough not to
/// demand a drum roll, short enough that two unrelated taps a second apart are
/// not read as one gesture.
const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(500);

use crate::dictionary::{check_and_correct, complete_candidates, Dict, Fix, Run};
use crate::keymap::{
    english_char_to_evkey_shifted, evkey_to_english_char, evkey_to_english_char_shifted,
    evkey_to_hebrew_char,
};
use crate::types::{
    lock_forgiving, AppControl, FixKind, Language, Replaceable, ReplaceGuard, WordBuffer,
};

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
    let held = st.held_keys.contains(&KeyCode::KEY_LEFTSHIFT)
        || st.held_keys.contains(&KeyCode::KEY_RIGHTSHIFT);
    held != st.caps_lock
}

/// Every device ReCast listens on: keyboards, plus mice so that a click can
/// reset the in-progress word buffer (parity with the macOS / Windows
/// `ButtonPress` handler). Its own injector is skipped.
///
/// A device the user has no permission to read is not returned at all —
/// `evdev::enumerate` cannot open it, so it never reaches the filter. That is
/// what makes an empty result the signal for "not in the `input` group"
/// rather than "no keyboard attached".
fn input_device_paths() -> Vec<std::path::PathBuf> {
    evdev::enumerate()
        .filter_map(|(path, dev)| {
            if dev.name() == Some("recast-injector") {
                return None;
            }
            let keys = dev.supported_keys();
            let is_keyboard = keys.is_some_and(|k| k.contains(KeyCode::KEY_A));
            let is_mouse = keys.is_some_and(|k| k.contains(KeyCode::BTN_LEFT));
            (is_keyboard || is_mouse).then_some(path)
        })
        .collect()
}

/// Everything that has to be true before the process detaches from the
/// terminal, checked while there is still a terminal to complain to.
///
/// `start` daemonizes by default, and daemonizing redirects stdout and stderr
/// to `/dev/null` (see `daemon::daemonize`). Both failures below are raised
/// *after* that point by `run`, which meant the overwhelmingly common first-run
/// problem — a user who is not in the `input` group — produced no output at
/// all: the shell prompt came straight back, the exit status was 0, and
/// nothing was ever corrected. The messages existed; nobody could read them.
fn preflight() -> Result<(), String> {
    // Injection. `builder()` is what opens `/dev/uinput`, so calling it is the
    // permission check; it is dropped again without `build()`, which means no
    // virtual device is registered — and no device-added event is sent to the
    // compositor — by the check itself.
    if let Err(e) = VirtualDevice::builder() {
        return Err(format!(
            "Cannot open /dev/uinput ({e}) — ReCast needs it to type corrections back.\n\
             Hint: load the module and give yourself access:\n\
             \x20 sudo modprobe uinput\n\
             \x20 echo 'KERNEL==\"uinput\", GROUP=\"input\", MODE=\"0660\"' \
             | sudo tee /etc/udev/rules.d/99-recast.rules\n\
             \x20 sudo udevadm control --reload-rules && sudo udevadm trigger"
        ));
    }

    // Capture.
    if input_device_paths().is_empty() {
        return Err(
            "No readable input devices — ReCast cannot see what you type.\n\
             Hint: sudo usermod -aG input $USER, then log out and back in."
                .to_string(),
        );
    }

    Ok(())
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
    // Before any of the three paths below, because two of them take the
    // terminal away: the daemon closes it, the TUI draws over it.
    if let Err(problem) = preflight() {
        eprintln!("{problem}");
        std::process::exit(1);
    }

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

    // `run` blocks for the life of the daemon — a normal shutdown is a signal,
    // which never gets here. Returning means it gave up: no injector, or every
    // device thread ended. Exiting 0 there told systemd the service had
    // finished its work, and `Restart=on-failure` (Makefile) left the unit
    // stopped instead of bringing it back.
    std::process::exit(1);
}

pub fn run(
    en_dict: Dict,
    he_dict: Dict,
    control: Arc<AppControl>,
) {
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
    crate::timing::pause(crate::timing::injection().device_settle);
    let injector = Arc::new(Mutex::new(injector));

    // Normally already checked by `preflight` before we detached from the
    // terminal; still handled here because a device can be unplugged in
    // between, and because `run` is also reached from the foreground UIs.
    let device_paths = input_device_paths();
    if device_paths.is_empty() {
        eprintln!("No input devices found. Make sure you are in the 'input' group.");
        eprintln!("Hint: Run 'sudo usermod -aG input $USER' and log out/in.");
        return;
    }

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
                    eprintln!("Could not open {path_clone:?}: {e}");
                    eprintln!(
                        "Hint: Are you in the 'input' group? \
                         Run 'sudo usermod -aG input $USER' and log out/in."
                    );
                    return;
                }
            };

            // A read error used to be retried forever. Unplugging a keyboard
            // does not make its node readable again — it makes every read fail
            // with the same error — so the thread settled into waking twice a
            // second, for the life of the daemon, to be told the device is
            // still gone. It also kept `run` from ever returning, because it
            // joins these handles.
            //
            // A handful of retries still covers what retrying is *for*: a
            // transient EINTR, or a device that drops out for a moment during
            // suspend/resume.
            const GIVE_UP_AFTER: u32 = 10;
            let mut failures = 0u32;

            loop {
                let events = match dev.fetch_events() {
                    Ok(ev) => {
                        failures = 0;
                        ev.collect::<Vec<_>>()
                    }
                    Err(e) => {
                        failures += 1;
                        eprintln!("Error reading {:?}: {}", path_clone, e);
                        if failures >= GIVE_UP_AFTER {
                            eprintln!(
                                "Giving up on {:?} after {} consecutive errors — \
                                 it is most likely unplugged.",
                                path_clone, failures
                            );
                            return;
                        }
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

    let mut st = lock_forgiving(state_mutex);

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
                    Run::default(),
                    en_dict,
                    he_dict,
                );

                // Describe the fix for the history before `replacement`
                // consumes it.
                let note = result.fix.as_ref().map(|fix| note_of(&st.keys, fix));
                if let Some(rep) = replacement(&st.keys, result.fix) {
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

    let mut st = lock_forgiving(state_mutex);
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
        // `.ready()`, not the old bare bool: "already on that layout" is a
        // reason to carry on, not to abandon the undo.
        if !crate::layout::switch_layout_to(lang).ready() {
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
        Run::default(),
        en_dict,
        he_dict,
    );
    let note = result.fix.as_ref().map(|fix| note_of(&skip.keys, fix));
    // Off the list, but the pipelines have nothing to say about it after all —
    // which is a fine outcome, and not one to rewrite the screen over.
    let Some(rep) = replacement(&skip.keys, result.fix) else {
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

/// Hand the correction to the kernel in pieces rather than in one write.
///
/// The whole sequence used to go out as a single `emit`, on the reasoning that
/// one locked write beats several. It does — but only for as long as the reader
/// keeps up. The buffer the events land in belongs to whoever is reading the
/// device, it holds 64 events, and a correction is `8 × word length + 8`: past
/// about seven letters a single write can fill it outright, and anything that
/// does not fit is dropped by the kernel rather than queued. The events emitted
/// last are the ones with nowhere to go, and the last thing a correction emits
/// is the space after the word.
///
/// Splitting the write does not make the buffer bigger. It bounds how much of
/// it we can occupy at once, and the gap in between is the reader's chance to
/// empty it — which is all that was missing.
fn emit_paced(injector: &Arc<Mutex<VirtualDevice>>, evs: &[evdev::InputEvent], gap: Duration) {
    let Ok(mut dev) = injector.lock() else {
        return;
    };
    let mut chunks = evs.chunks(crate::timing::EVENTS_PER_WRITE).peekable();
    while let Some(chunk) = chunks.next() {
        if dev.emit(chunk).is_err() {
            return;
        }
        // Only *between* writes. A gap after the last one would delay nothing
        // but the release of the injector lock.
        if chunks.peek().is_some() {
            crate::timing::pause(gap);
        }
    }
}

/// Erase `erase` characters the user typed and inject a replacement: the same
/// keys after a layout switch, different keys for a spelling fix, or the rest of
/// the word for a completion.
///
/// `terminator` is the key that ended the word (space/enter) and gets pressed
/// again after the replacement. It is `None` for a completion, which is
/// triggered by a key that types nothing and therefore has nothing to restore.
/// Pressing it again means waiting for the user to lift it first — see 1b — so
/// a correction lands when the space comes up rather than when it goes down.
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

    // Armed for the whole replacement: whatever happens below — including a
    // panic — `is_replacing` is cleared and the buffered keys are dropped,
    // rather than leaving the listener gated shut for the rest of the session.
    let _gate = ReplaceGuard::new(state_mutex.as_ref(), None);

    let syn = || {
        InputEvent::new(
            EventType::SYNCHRONIZATION.0,
            SynchronizationCode::SYN_REPORT.0,
            0,
        )
    };

    let gaps = crate::timing::injection();
    // Wait for every key in `keys` to be physically up, or for `ceiling` to
    // pass. A ceiling, not a cost: it returns the moment the last one lifts.
    let wait_for_release = |keys: &HashSet<KeyCode>, ceiling: Duration| {
        let start = Instant::now();
        loop {
            let held = {
                let st = lock_forgiving(state_mutex);
                keys.iter().any(|k| st.held_keys.contains(k))
            };
            if !held || start.elapsed() >= ceiling {
                return;
            }
            crate::timing::pause(gaps.held_poll);
        }
    };

    // 1a. Only the keys we are about to *press* matter here: a press injected
    //     while the physical key is still down never reaches the focused
    //     window — the compositor already has that key down and discards the
    //     second press as a duplicate. The erased keys are not in that set;
    //     they only cost backspaces.
    //
    //     There is no injecting our way out of it. Sending a *release* first —
    //     which is what this did when the wait ran out — does nothing at all:
    //     the kernel tracks key state per input device, and this device never
    //     pressed the key, so the release is discarded before any compositor
    //     sees it. Measured against Hyprland, a press from the injector stays
    //     swallowed for as long as the real key is down, however many
    //     press/release pairs are sent after it. Only the user's own finger
    //     clears it, which leaves waiting as the only thing that works.
    let mut retyped_keys: HashSet<KeyCode> = retype.iter().map(|(k, _)| *k).collect();
    //     Both shifts are always in the set. We inject each key with exactly the
    //     shift state we decided on — none at all for a Hebrew target, where
    //     shift types punctuation rather than a capital — so a shift the *user*
    //     happens to still be holding has to come up first, and one we press
    //     ourselves would be a duplicate of it.
    retyped_keys.insert(KC::KEY_LEFTSHIFT);
    retyped_keys.insert(KC::KEY_RIGHTSHIFT);
    wait_for_release(&retyped_keys, gaps.held_release_timeout);

    // 1b. The terminator gets its own, much longer ceiling, because it is the
    //     one key that is *always* still down here: pressing it is what asked
    //     for the correction, and the general ceiling is a fraction of an
    //     ordinary keypress. Giving up on it early is what left the corrected
    //     word with no space after it — every time, for anyone who does not
    //     type in taps.
    //
    //     This is the one wait the user can feel, and it ends when they lift a
    //     key they were lifting anyway to type the next word.
    let terminator_key: HashSet<KeyCode> = terminator.into_iter().collect();
    wait_for_release(&terminator_key, gaps.terminator_release_timeout);

    // Clone buffered keys while holding the lock, then release it before injecting
    // any keys. The injected keystrokes re-enter handle_key which also needs the
    // state lock, so holding it here would cause a deadlock that silently drops
    // the injected space/terminator.
    let buffered = {
        let st = lock_forgiving(state_mutex);
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

    emit_paced(injector, &evs, gaps.batch_gap);

    // Re-acquire the lock only to clean up state.
    let mut st = lock_forgiving(state_mutex);
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
    // Reset the dedup guard so the injected terminator (space/enter) is not
    // silently dropped because it shares the same keycode as the physical
    // keypress that triggered this replacement (both arrive within 5 ms).
    st.last_keycode = None;
    // `buffered_keys` and `is_replacing` are the guard's, and it clears them
    // after this lock is dropped — on this path and on a panicking one alike.
}
