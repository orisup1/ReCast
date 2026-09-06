use super::engine::{self, Engine, Plan, Platform};
use crate::dictionary::Dict;
use crate::keymap::{
    english_char_to_evkey_shifted, evkey_to_english_char, evkey_to_english_char_shifted,
    evkey_to_hebrew_char,
};
use crate::types::{AppControl, Language};
use evdev::{uinput::VirtualDevice, AttributeSet, Device, EventSummary, KeyCode};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
type Typed = engine::Typed<KeyCode>;

pub struct Linux;
impl Platform for Linux {
    type Key = KeyCode;
    type Retype = Vec<(KeyCode, bool)>;
    type Injector = Arc<Mutex<VirtualDevice>>;
    type Focus = String;
    const SHIFT_LEFT: KeyCode = KeyCode::KEY_LEFTSHIFT;
    const SHIFT_RIGHT: KeyCode = KeyCode::KEY_RIGHTSHIFT;
    const CTRL_LEFT: KeyCode = KeyCode::KEY_LEFTCTRL;
    const CTRL_RIGHT: KeyCode = KeyCode::KEY_RIGHTCTRL;
    const CAPS_LOCK: KeyCode = KeyCode::KEY_CAPSLOCK;
    const BACKSPACE: KeyCode = KeyCode::KEY_BACKSPACE;
    const DEDUP_WINDOW: Option<Duration> = Some(Duration::from_millis(5));
    const ABORT_UNDO_IF_LAYOUT_REFUSED: bool = true;
    fn is_terminator(key: KeyCode) -> bool {
        matches!(
            key,
            KeyCode::KEY_SPACE | KeyCode::KEY_ENTER | KeyCode::KEY_KPENTER
        )
    }
    fn is_reset(key: KeyCode) -> bool {
        use KeyCode as K;
        matches!(
            key,
            K::KEY_TAB
                | K::KEY_ESC
                | K::KEY_LEFT
                | K::KEY_RIGHT
                | K::KEY_UP
                | K::KEY_DOWN
                | K::KEY_HOME
                | K::KEY_END
                | K::KEY_PAGEUP
                | K::KEY_PAGEDOWN
                | K::KEY_INSERT
                | K::KEY_DELETE
                | K::BTN_LEFT
                | K::BTN_RIGHT
                | K::BTN_MIDDLE
        )
    }
    fn is_modifier(key: KeyCode) -> bool {
        use KeyCode as K;
        matches!(
            key,
            K::KEY_LEFTSHIFT
                | K::KEY_RIGHTSHIFT
                | K::KEY_LEFTCTRL
                | K::KEY_RIGHTCTRL
                | K::KEY_LEFTALT
                | K::KEY_RIGHTALT
                | K::KEY_LEFTMETA
                | K::KEY_RIGHTMETA
                | K::KEY_CAPSLOCK
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
    fn retype_original(keys: &[Typed], lang: Language) -> Self::Retype {
        keys.iter()
            .map(|t| (t.key, t.shift && lang == Language::English))
            .collect()
    }
    fn retype_layout(keys: &[Typed], _: &str, lang: Language) -> Option<Self::Retype> {
        Some(Self::retype_original(keys, lang))
    }
    fn retype_text(text: &str) -> Option<Self::Retype> {
        text.chars().map(english_char_to_evkey_shifted).collect()
    }
    fn retype_len(retype: &Self::Retype) -> usize {
        retype.len()
    }
    fn buffer_after(retype: &Self::Retype) -> Vec<Typed> {
        retype
            .rsplit(|(key, _)| *key == KeyCode::KEY_SPACE)
            .next()
            .unwrap_or_default()
            .iter()
            .map(|&(key, shift)| Typed { key, shift })
            .collect()
    }
    fn injecting_flag(_: &Self::Injector) -> Option<&std::sync::atomic::AtomicBool> {
        None
    }
    fn focus() -> Option<String> {
        crate::layout::focused_target()
    }
    fn inject(engine: &Engine<Self>, plan: Plan<Self>, generation: u64) -> Option<Vec<Typed>> {
        inject(engine, plan, generation)
    }
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

    // Under systemd (Type=simple), keep the PID the service manager tracks.
    // Fork before starting workers; otherwise their threads disappear and
    // their locks may remain held forever in the child.
    let under_systemd = std::env::var_os("INVOCATION_ID").is_some();
    if !with_window && !with_gui && !with_foreground && !under_systemd {
        crate::daemon::daemonize();
    }
    super::start_background_tasks();

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

    let engine = Engine::<Linux>::new(en_dict, he_dict, control, injector);

    let mut handles = vec![];

    for path in device_paths {
        let engine = Arc::clone(&engine);
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

fn emit_paced(
    injector: &Arc<Mutex<VirtualDevice>>,
    evs: &[evdev::InputEvent],
    gap: Duration,
) -> Option<()> {
    let mut dev = injector.lock().ok()?;
    let mut chunks = evs.chunks(crate::timing::EVENTS_PER_WRITE).peekable();
    while let Some(chunk) = chunks.next() {
        dev.emit(chunk).ok()?;
        // Only *between* writes. A gap after the last one would delay nothing
        // but the release of the injector lock.
        if chunks.peek().is_some() {
            crate::timing::pause(gap);
        }
    }
    Some(())
}

fn inject(engine: &Engine<Linux>, plan: Plan<Linux>, generation: u64) -> Option<Vec<Typed>> {
    use evdev::{EventType, InputEvent, KeyCode as KC, SynchronizationCode};
    let Plan {
        erase,
        retype,
        terminator,
    } = plan;
    let syn = || {
        InputEvent::new(
            EventType::SYNCHRONIZATION.0,
            SynchronizationCode::SYN_REPORT.0,
            0,
        )
    };

    let gaps = crate::timing::injection();
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
    engine.wait_for_release(
        &retyped_keys.into_iter().collect::<Vec<_>>(),
        gaps.held_release_timeout,
    );

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
    engine.wait_for_release(
        &terminator_key.into_iter().collect::<Vec<_>>(),
        gaps.terminator_release_timeout,
    );

    // Clone buffered keys while holding the lock, then release it before injecting
    // any keys. The injected keystrokes re-enter handle_key which also needs the
    // state lock, so holding it here would cause a deadlock that silently drops
    // the injected space/terminator.
    if !engine.replacement_valid(generation) {
        return None;
    }
    let buffered = engine.buffered();

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

    if !engine.replacement_valid(generation) {
        return None;
    }
    emit_paced(&engine.injector, &evs, gaps.batch_gap)?;

    Some(buffered)
}
