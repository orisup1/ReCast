use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use evdev::{uinput::VirtualDevice, AttributeSet, Device, EventSummary, KeyCode};

/// Maximum time `replace_word` will wait for the user to physically release
/// the keys we are about to retype before injecting anyway.
const HELD_RELEASE_TIMEOUT: Duration = Duration::from_millis(150);

use crate::dictionary::check_and_switch_with_split;
use crate::keymap::{evkey_to_english_char, evkey_to_hebrew_char};
use crate::types::AppControl;

/// Per-keyboard state tracked across events.
pub struct AppState {
    pub keys: Vec<KeyCode>,
    pub last_event_time: Instant,
    pub last_keycode: Option<KeyCode>,
    pub is_replacing: bool,
    pub buffered_keys: Vec<KeyCode>,
    /// Physical keys currently held down. Tracked from press/release events
    /// so the replace_word thread can wait for the user to lift the keys it
    /// is about to retype — otherwise the compositor squashes our synthetic
    /// press as a duplicate of the still-held physical key.
    pub held_keys: HashSet<KeyCode>,
}

pub fn run(en_dict: HashSet<String>, he_dict: HashSet<String>, control: Arc<AppControl>) {
    println!("Starting recast keyboard watcher (Linux/Wayland)...");

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
            let is_keyboard = keys.map_or(false, |k| k.contains(KeyCode::KEY_A));
            let is_mouse = keys.map_or(false, |k| k.contains(KeyCode::BTN_LEFT));
            if is_keyboard || is_mouse {
                Some(path)
            } else {
                None
            }
        })
        .collect();

    if device_paths.is_empty() {
        eprintln!("No input devices found. Make sure you are in the 'input' group.");
        return;
    }

    println!("Found {} input device(s).", device_paths.len());

    let state = Arc::new(Mutex::new(AppState {
        keys: Vec::new(),
        last_event_time: Instant::now(),
        last_keycode: None,
        is_replacing: false,
        buffered_keys: Vec::new(),
        held_keys: HashSet::new(),
    }));

    let en_dict = Arc::new(en_dict);
    let he_dict = Arc::new(he_dict);

    let mut handles = vec![];

    for path in device_paths {
        let state = Arc::clone(&state);
        let en_dict = Arc::clone(&en_dict);
        let he_dict = Arc::clone(&he_dict);
        let injector = Arc::clone(&injector);
        let control = Arc::clone(&control);
        let path_clone = path.clone();

        let handle = thread::spawn(move || {
            let mut dev = match Device::open(&path_clone) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Could not open {:?}: {}", path_clone, e);
                    return;
                }
            };

            println!("Passively listening on {:?}", path_clone);

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
                                keycode, &state, &en_dict, &he_dict, &injector, &control,
                            ),
                            0 => {
                                // Track release so replace_word can wait until the
                                // user has actually lifted the keys it needs to retype.
                                let mut st = state.lock().unwrap();
                                st.held_keys.remove(&keycode);
                            }
                            _ => {}
                        }
                    }
                }
            }
        });
        handles.push(handle);
    }

    println!("Listening for keyboard events. Press Space or Enter to check a word.");
    for h in handles {
        let _ = h.join();
    }
}

/// Process a single key-press event.
fn handle_key(
    key: KeyCode,
    state_mutex: &Arc<Mutex<AppState>>,
    en_dict: &HashSet<String>,
    he_dict: &HashSet<String>,
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

    match key {
        KC::KEY_SPACE | KC::KEY_ENTER | KC::KEY_KPENTER => {
            if st.is_replacing {
                st.buffered_keys.push(key);
                return;
            }

            if !st.keys.is_empty() {
                if !control.is_enabled() {
                    st.keys.clear();
                    return;
                }
                let result = check_and_switch_with_split(
                    &st.keys,
                    evkey_to_english_char,
                    evkey_to_hebrew_char,
                    en_dict,
                    he_dict,
                );

                if let Some(start) = result {
                    control.record_fix();
                    st.is_replacing = true;
                    // Only the suffix from `start` onward is the word that
                    // needs to be erased and retyped — anything before it is
                    // a previously-typed word that the user concatenated by
                    // forgetting a space, and we want to leave it intact.
                    let keys_clone: Vec<KeyCode> = st.keys[start..].to_vec();
                    let terminator = key;
                    let injector_clone = Arc::clone(injector);
                    let state_clone = Arc::clone(state_mutex);
                    thread::spawn(move || {
                        replace_word(keys_clone, terminator, &injector_clone, &state_clone);
                    });
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
                    st.buffered_keys.push(key);
                } else {
                    st.keys.push(key);
                }
            }
        }
    }
}

/// After a layout switch, erase the mistyped word and retype it in the new layout.
fn replace_word(
    keys: Vec<KeyCode>,
    terminator: KeyCode,
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

    // 1a. Wait for the user to physically release the terminator and any of
    //     the word's keys. If we inject a synthetic press while the same key
    //     is still held by the physical keyboard, the compositor sees it as
    //     a duplicate of the held key and drops it — which is why the
    //     trailing space (and occasionally the last word letter) went missing.
    let mut keys_of_interest: HashSet<KeyCode> = keys.iter().copied().collect();
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
        st.buffered_keys.clone()
    };

    // Build the whole erase+retype sequence as one event batch and emit it in
    // a single locked write. Each key is press / SYN / release / SYN so the
    // compositor still sees distinct keystroke frames, but there are no
    // inter-key sleeps and the injector lock is taken once instead of twice
    // per event — the dominant cost of retyping.
    let delete_count = keys.len() + 1 + buffered.len(); // +1 = physical terminator
    let total_keys = delete_count + keys.len() + 1 + buffered.len();
    let mut evs: Vec<InputEvent> = Vec::with_capacity(total_keys * 4);
    let mut push_key = |kc: KC| {
        evs.push(InputEvent::new(EventType::KEY.0, kc.0, 1));
        evs.push(syn());
        evs.push(InputEvent::new(EventType::KEY.0, kc.0, 0));
        evs.push(syn());
    };

    // 2. Erase the word + buffered keys.
    for _ in 0..delete_count {
        push_key(KC::KEY_BACKSPACE);
    }
    // 3. Retype the physical keys.
    for key in &keys {
        push_key(*key);
    }
    // 4. Retype the terminator (space/enter).
    push_key(terminator);
    // 5. Retype buffered keys.
    for key in &buffered {
        push_key(*key);
    }

    if let Ok(mut dev) = injector.lock() {
        let _ = dev.emit(&evs);
    }

    // Re-acquire the lock only to clean up state.
    let mut st = state_mutex.lock().unwrap();
    st.keys = buffered.clone();
    st.buffered_keys.clear();
    st.is_replacing = false;
    // Reset the dedup guard so the injected terminator (space/enter) is not
    // silently dropped because it shares the same keycode as the physical
    // keypress that triggered this replacement (both arrive within 5 ms).
    st.last_keycode = None;
}
