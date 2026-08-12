//! Linux: `evdev` capture, `uinput` injection, and the daemon/GUI/TUI startup
//! path.
//!
//! Everything between capture and injection — the word buffer, both gestures,
//! undo, the completion cycle — is [`crate::platform::engine`]. What is left
//! here is what only Linux does:
//!
//! * read from every keyboard and mouse node directly, one thread each;
//! * inject **keycodes**, because `uinput` has no way to insert text, which is
//!   why [`Platform::Retype`] is a key list here and a string on the other two;
//! * daemonize, or hand the main thread to the control window or the TUI.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use evdev::{uinput::VirtualDevice, AttributeSet, Device, EventSummary, KeyCode};

use super::engine::{Engine, Plan, Platform, Typed};
use crate::dictionary::Dict;
use crate::keymap::{
    english_char_to_evkey_shifted, evkey_to_english_char, evkey_to_english_char_shifted,
    evkey_to_hebrew_char,
};
use crate::types::{AppControl, Language};

/// One physical press arrives on several evdev nodes — the device's own and
/// whatever aggregate node the kernel also exposes — so the same key again
/// inside this window is the same press, not a second one. macOS and Windows
/// have a single event stream and need no such guard.
const DEDUP_WINDOW: Duration = Duration::from_millis(5);

/// What Linux injects: key positions with the shift to hold around them.
///
/// `uinput` speaks keycodes, so there is no way to insert a finished word the
/// way macOS and Windows do. Replaying keys is equivalent for a layout fix —
/// the layout has already changed, so they now produce that same text — and the
/// whole sequence goes out as one batch, so it still lands in a single frame
/// rather than as visible retyping.
type Keys = Vec<(KeyCode, bool)>;

pub struct Linux;

impl Platform for Linux {
    type Key = KeyCode;
    type Retype = Keys;
    type Injector = Arc<Mutex<VirtualDevice>>;

    const SHIFT_LEFT: KeyCode = KeyCode::KEY_LEFTSHIFT;
    const SHIFT_RIGHT: KeyCode = KeyCode::KEY_RIGHTSHIFT;
    const CTRL_LEFT: KeyCode = KeyCode::KEY_LEFTCTRL;
    const CTRL_RIGHT: KeyCode = KeyCode::KEY_RIGHTCTRL;
    const CAPS_LOCK: KeyCode = KeyCode::KEY_CAPSLOCK;
    const BACKSPACE: KeyCode = KeyCode::KEY_BACKSPACE;

    const DEDUP_WINDOW: Option<Duration> = Some(DEDUP_WINDOW);
    const ABORT_UNDO_IF_LAYOUT_REFUSED: bool = true;

    fn is_terminator(key: KeyCode) -> bool {
        matches!(
            key,
            KeyCode::KEY_SPACE | KeyCode::KEY_ENTER | KeyCode::KEY_KPENTER
        )
    }

    /// Cursor and focus keys, plus the mouse buttons — which arrive here as
    /// ordinary keys on the same evdev stream, where macOS and Windows get
    /// their own event kind for them.
    fn is_reset(key: KeyCode) -> bool {
        matches!(
            key,
            KeyCode::KEY_TAB
                | KeyCode::KEY_ESC
                | KeyCode::KEY_LEFT
                | KeyCode::KEY_RIGHT
                | KeyCode::KEY_UP
                | KeyCode::KEY_DOWN
                | KeyCode::KEY_HOME
                | KeyCode::KEY_END
                | KeyCode::KEY_PAGEUP
                | KeyCode::KEY_PAGEDOWN
                | KeyCode::KEY_INSERT
                | KeyCode::KEY_DELETE
                | KeyCode::BTN_LEFT
                | KeyCode::BTN_RIGHT
                | KeyCode::BTN_MIDDLE
        )
    }

    fn english_char(key: KeyCode, shift: bool) -> Option<char> {
        evkey_to_english_char_shifted(key, shift)
    }

    fn english_char_plain(key: KeyCode) -> Option<char> {
        evkey_to_english_char(key)
    }

    fn hebrew_char(key: KeyCode) -> Option<char> {
        evkey_to_hebrew_char(key)
    }

    /// The user's own keys, replayed — which is what makes an irreproducible
    /// capitalisation (`sHiFtY`) survive an undo.
    ///
    /// The shift state is replayed with them only when the target is English:
    /// that is what puts the capital back on a mistyped `Shalom`. Hebrew has no
    /// capitals, and shift there types punctuation, so a Hebrew target is
    /// replayed unshifted whatever the user held.
    fn retype_original(keys: &[Typed<KeyCode>], lang: Language) -> Keys {
        let keep_shift = lang == Language::English;
        keys.iter()
            .map(|t| (t.key, t.shift && keep_shift))
            .collect()
    }

    /// Same keys, new layout — they now produce the other language. `text` is
    /// unused: a `uinput` device speaks keycodes, not characters.
    fn retype_layout(keys: &[Typed<KeyCode>], _text: &str, lang: Language) -> Option<Keys> {
        Some(Self::retype_original(keys, lang))
    }

    /// Spell `text` out as key presses, or `None` if any character can't be
    /// typed under the English layout — better to drop the fix than inject half
    /// a word.
    fn retype_text(text: &str) -> Option<Keys> {
        text.chars().map(english_char_to_evkey_shifted).collect()
    }

    fn retype_len(retype: &Keys) -> usize {
        retype.len()
    }

    /// What is on screen for the word still in progress. An expansion may carry
    /// spaces, in which case only the last word of it is unfinished.
    fn buffer_after(retype: &Keys) -> Vec<Typed<KeyCode>> {
        retype
            .rsplit(|(k, _)| *k == KeyCode::KEY_SPACE)
            .next()
            .unwrap_or_default()
            .iter()
            .map(|&(key, shift)| Typed { key, shift })
            .collect()
    }

    /// Linux filters its own injected events by device name
    /// (`recast-injector`), so there is no atomic gate to clear.
    fn injecting_flag(_injector: &Self::Injector) -> Option<&std::sync::atomic::AtomicBool> {
        None
    }

    fn inject(engine: &Engine<Self>, plan: Plan<Self>) -> Vec<Typed<KeyCode>> {
        use evdev::{EventType, InputEvent, SynchronizationCode};

        let gaps = crate::timing::injection();

        // 1a. Only the keys we are about to *press* matter here: a press
        //     injected while the physical key is still down never reaches the
        //     focused window. The erased keys are not in that set; they only
        //     cost backspaces.
        //
        //     Both shifts are always in it. Each key is injected with exactly
        //     the shift state decided on — none at all for a Hebrew target,
        //     where shift types punctuation rather than a capital — so a shift
        //     the *user* happens to still be holding has to come up first, and
        //     one we press ourselves would be a duplicate of it.
        let mut wait_for: Vec<KeyCode> = plan.retype.iter().map(|(k, _)| *k).collect();
        wait_for.push(KeyCode::KEY_LEFTSHIFT);
        wait_for.push(KeyCode::KEY_RIGHTSHIFT);
        engine.wait_for_release(&wait_for, gaps.held_release_timeout);

        // 1b. The terminator gets its own, much longer ceiling, because it is
        //     the one key that is *always* still down here: pressing it is what
        //     asked for the correction, and the general ceiling is a fraction
        //     of an ordinary keypress. Giving up on it early is what left the
        //     corrected word with no space after it — every time, for anyone
        //     who does not type in taps.
        //
        //     This is the one wait the user can feel, and it ends when they
        //     lift a key they were lifting anyway to type the next word.
        let terminator: Vec<KeyCode> = plan.terminator.into_iter().collect();
        engine.wait_for_release(&terminator, gaps.terminator_release_timeout);

        let buffered = engine.buffered();

        // Build the whole erase+retype sequence as one event batch. Each key is
        // press / SYN / release / SYN so the compositor still sees distinct
        // keystroke frames, but there are no inter-key sleeps and the injector
        // lock is taken once instead of twice per event — the dominant cost of
        // retyping.
        let syn = || {
            InputEvent::new(
                EventType::SYNCHRONIZATION.0,
                SynchronizationCode::SYN_REPORT.0,
                0,
            )
        };
        let delete_count = plan.erase + buffered.len();
        let total = delete_count + plan.retype.len() + 1 + buffered.len();
        let mut evs: Vec<InputEvent> = Vec::with_capacity(total * 8);

        // A capital is Left Shift held around the letter — the only way to type
        // one through a device that speaks key positions.
        let mut push_char = |kc: KeyCode, shift: bool| {
            if shift {
                evs.push(InputEvent::new(
                    EventType::KEY.0,
                    KeyCode::KEY_LEFTSHIFT.0,
                    1,
                ));
                evs.push(syn());
            }
            evs.push(InputEvent::new(EventType::KEY.0, kc.0, 1));
            evs.push(syn());
            evs.push(InputEvent::new(EventType::KEY.0, kc.0, 0));
            evs.push(syn());
            if shift {
                evs.push(InputEvent::new(
                    EventType::KEY.0,
                    KeyCode::KEY_LEFTSHIFT.0,
                    0,
                ));
                evs.push(syn());
            }
        };

        // 2. Erase the word + buffered keys.
        for _ in 0..delete_count {
            push_char(KeyCode::KEY_BACKSPACE, false);
        }
        // 3. Type the corrected word.
        for (key, shift) in &plan.retype {
            push_char(*key, *shift);
        }
        // 4. Retype the terminator (space/enter) — a completion has none.
        if let Some(terminator) = plan.terminator {
            push_char(terminator, false);
        }
        // 5. Retype buffered keys.
        for t in &buffered {
            push_char(t.key, t.shift);
        }

        emit_paced(&engine.injector, &evs, gaps.batch_gap);
        buffered
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

// ─────────────────────────────────────────────────────────────────────────────
// Capture
// ─────────────────────────────────────────────────────────────────────────────

/// Every device ReCast listens on: keyboards, plus mice so that a click can
/// reset the in-progress word buffer. Its own injector is skipped.
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

pub fn run(en_dict: Dict, he_dict: Dict, control: Arc<AppControl>) {
    // Persistent virtual device strictly for injecting backspaces and
    // corrected words. Created once so Wayland has time to recognise it.
    let mut all_keys = AttributeSet::<KeyCode>::new();
    for code in 0u16..=249 {
        all_keys.insert(KeyCode::new(code));
    }

    let builder = match VirtualDevice::builder() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to create injector builder: {e}");
            return;
        }
    };

    let injector = match builder.name("recast-injector").with_keys(&all_keys) {
        Ok(b) => match b.build() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Failed to build injector device: {e}");
                return;
            }
        },
        Err(e) => {
            eprintln!("Failed to configure injector device: {e}");
            return;
        }
    };

    // Allow time for the OS/compositor to detect the new injection device.
    crate::timing::pause(crate::timing::injection().device_settle);

    // Normally already checked by `preflight` before we detached from the
    // terminal; still handled here because a device can be unplugged in
    // between, and because `run` is also reached from the foreground UIs.
    let device_paths = input_device_paths();
    if device_paths.is_empty() {
        eprintln!("No input devices found. Make sure you are in the 'input' group.");
        eprintln!("Hint: Run 'sudo usermod -aG input $USER' and log out/in.");
        return;
    }

    let engine = Engine::<Linux>::new(en_dict, he_dict, control, Arc::new(Mutex::new(injector)));

    let mut handles = vec![];
    for path in device_paths {
        let engine = Arc::clone(&engine);
        let handle = thread::spawn(move || {
            let mut dev = match Device::open(&path) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Could not open {path:?}: {e}");
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
                        eprintln!("Error reading {path:?}: {e}");
                        if failures >= GIVE_UP_AFTER {
                            eprintln!(
                                "Giving up on {path:?} after {failures} consecutive errors — \
                                 it is most likely unplugged."
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
                            1 => engine.key_press(keycode),
                            0 => engine.key_release(keycode),
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
