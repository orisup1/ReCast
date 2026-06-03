use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rdev::{listen, simulate, Event, EventType, Key};

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

pub fn run(en_dict: HashSet<String>, he_dict: HashSet<String>, control: Arc<AppControl>) {
    println!("Starting recast keyboard watcher (Windows)...");

    let en_dict_cb = en_dict.clone();
    let he_dict_cb = he_dict.clone();
    let control_cb = Arc::clone(&control);
    let state: Arc<Mutex<AppState>> = Arc::new(Mutex::new(AppState {
        keys: Vec::new(),
        is_replacing: false,
        buffered_keys: Vec::new(),
        held_keys: HashSet::new(),
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
                match key {
                    Key::Space | Key::Return => {
                        if st.is_replacing {
                            st.buffered_keys.push(key);
                            return;
                        }

                        if !st.keys.is_empty() {
                            if !control_cb.is_enabled() {
                                st.keys.clear();
                                return;
                            }
                            let result = check_and_switch_with_split(
                                &st.keys,
                                key_to_english_char,
                                key_to_hebrew_char,
                                &en_dict_cb,
                                &he_dict_cb,
                            );

                            if let Some(start) = result {
                                control_cb.record_fix();
                                st.is_replacing = true;
                                // See linux.rs: anything before `start` is a
                                // previously-typed word the user concatenated
                                // by forgetting a space; leave it untouched.
                                let keys_clone: Vec<Key> = st.keys[start..].to_vec();
                                let terminator = key;
                                let state_clone = Arc::clone(&state_cb);
                                let injecting_flag = Arc::clone(&injecting);

                                thread::spawn(move || {
                                    replace_word(
                                        keys_clone,
                                        terminator,
                                        &state_clone,
                                        &injecting_flag,
                                    );
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
                                st.buffered_keys.push(key);
                            } else {
                                st.keys.push(key);
                            }
                        }
                    }
                }
            }
            EventType::KeyRelease(key) => {
                st.held_keys.remove(&key);
            }
            EventType::ButtonPress(_) => {
                if st.is_replacing {
                    st.buffered_keys.clear();
                } else {
                    st.keys.clear();
                }
            }
            _ => {}
        }
    };

    println!("Listening for keyboard events. Press Space or Enter to check a word.");
    if let Err(err) = listen(callback) {
        eprintln!("Error while listening for keyboard events: {:?}", err);
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

    // 2. switch_layout_to already polled until the layout change took effect,
    //    so no settle delay is needed here.

    // 3. Gate the listener now that we are about to inject our own events.
    injecting.store(true, Ordering::Relaxed);

    let buf = {
        let st = state_mutex.lock().unwrap();
        st.buffered_keys.clone()
    };

    // +1 for the terminator the user physically typed.
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
