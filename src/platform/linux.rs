use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use evdev::{uinput::VirtualDevice, AttributeSet, Device, EventSummary, KeyCode};

use crate::dictionary::check_and_switch_candidates;
use crate::keymap::{evkey_to_english_char, evkey_to_hebrew_char};

/// Per-keyboard state tracked across events.
pub struct AppState {
    pub keys: Vec<KeyCode>,
    pub last_event_time: Instant,
    pub last_keycode: Option<KeyCode>,
}

pub fn run(en_dict: HashSet<String>, he_dict: HashSet<String>) {
    println!("Starting typeLan keyboard watcher (Linux/Wayland)...");

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

    let injector = match builder.name("typeLan-injector").with_keys(&all_keys) {
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

    // Find all physical keyboard devices.
    let keyboard_paths: Vec<std::path::PathBuf> = evdev::enumerate()
        .filter_map(|(path, dev)| {
            if dev.name() == Some("typeLan-injector") {
                return None;
            }
            if dev
                .supported_keys()
                .map_or(false, |k| k.contains(KeyCode::KEY_A))
            {
                Some(path)
            } else {
                None
            }
        })
        .collect();

    if keyboard_paths.is_empty() {
        eprintln!("No keyboard devices found. Make sure you are in the 'input' group.");
        return;
    }

    println!("Found {} keyboard device(s).", keyboard_paths.len());

    let state = Arc::new(Mutex::new(AppState {
        keys: Vec::new(),
        last_event_time: Instant::now(),
        last_keycode: None,
    }));

    let en_dict = Arc::new(en_dict);
    let he_dict = Arc::new(he_dict);

    let mut handles = vec![];

    for path in keyboard_paths {
        let state = Arc::clone(&state);
        let en_dict = Arc::clone(&en_dict);
        let he_dict = Arc::clone(&he_dict);
        let injector = Arc::clone(&injector);
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
                        // Only process key presses (value == 1).
                        if value == 1 {
                            handle_key(keycode, &state, &en_dict, &he_dict, &injector);
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

    match key {
        KC::KEY_SPACE | KC::KEY_ENTER | KC::KEY_KPENTER => {
            if !st.keys.is_empty() {
                let word_en: String = st
                    .keys
                    .iter()
                    .filter_map(|&k| evkey_to_english_char(k))
                    .collect();
                let word_he: String = st
                    .keys
                    .iter()
                    .filter_map(|&k| evkey_to_hebrew_char(k))
                    .collect();

                let switched =
                    check_and_switch_candidates(&word_en, &word_he, en_dict, he_dict);

                if switched {
                    let keys_clone = st.keys.clone();
                    let terminator = key;
                    let injector_clone = Arc::clone(injector);
                    thread::spawn(move || {
                        replace_word(keys_clone, terminator, &injector_clone);
                    });
                }
                st.keys.clear();
            }
        }
        KC::KEY_BACKSPACE => {
            st.keys.pop();
        }
        _ => {
            if evkey_to_english_char(key).is_some() || evkey_to_hebrew_char(key).is_some() {
                st.keys.push(key);
            }
        }
    }
}

/// After a layout switch, erase the mistyped word and retype it in the new layout.
fn replace_word(
    keys: Vec<KeyCode>,
    terminator: KeyCode,
    injector: &Arc<Mutex<VirtualDevice>>,
) {
    use evdev::{EventType, InputEvent, KeyCode as KC, SynchronizationCode};

    let emit = |kc: KC, val: i32| {
        let evs = [
            InputEvent::new(EventType::KEY.0, kc.0, val),
            InputEvent::new(
                EventType::SYNCHRONIZATION.0,
                SynchronizationCode::SYN_REPORT.0,
                0,
            ),
        ];
        if let Ok(mut dev) = injector.lock() {
            let _ = dev.emit(&evs);
        }
    };

    let press_release = |kc: KC| {
        emit(kc, 1);
        thread::sleep(Duration::from_millis(5));
        emit(kc, 0);
        thread::sleep(Duration::from_millis(5));
    };

    // 1. Wait for the hyprctl layout switch to take effect in the compositor.
    thread::sleep(Duration::from_millis(120));

    // 2. Erase the word (+1 for the terminator the user physically typed).
    let delete_count = keys.len() + 1;
    for _ in 0..delete_count {
        press_release(KC::KEY_BACKSPACE);
    }

    // Slight pause between erase and retype.
    thread::sleep(Duration::from_millis(20));

    // 3. Retype the physical keys.
    for key in &keys {
        press_release(*key);
    }

    // 4. Retype the terminator.
    press_release(terminator);
}
