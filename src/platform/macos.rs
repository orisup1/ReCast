use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rdev::{listen, simulate, Event, EventType, Key};

use crate::dictionary::check_and_switch_candidates;
use crate::keymap::{key_to_english_char, key_to_hebrew_char};

pub fn run(en_dict: HashSet<String>, he_dict: HashSet<String>) {
    println!("Starting typeLan keyboard watcher (macOS)...");

    let en_dict_cb = en_dict.clone();
    let he_dict_cb = he_dict.clone();
    let current_keys: Arc<Mutex<Vec<Key>>> = Arc::new(Mutex::new(Vec::new()));
    let keys_cb = Arc::clone(&current_keys);

    let callback = move |event: Event| {
        let mut keys = keys_cb.lock().unwrap();
        match event.event_type {
            EventType::KeyPress(key) => match key {
                Key::Space | Key::Return => {
                    if !keys.is_empty() {
                        let word_en: String =
                            keys.iter().filter_map(|&k| key_to_english_char(k)).collect();
                        let word_he: String =
                            keys.iter().filter_map(|&k| key_to_hebrew_char(k)).collect();
                        let switched = check_and_switch_candidates(
                            &word_en,
                            &word_he,
                            &en_dict_cb,
                            &he_dict_cb,
                        );
                        if switched {
                            let keys_clone = keys.clone();
                            thread::spawn(move || {
                                thread::sleep(Duration::from_millis(50));
                                let delete_count = keys_clone.len() + 1;
                                for _ in 0..delete_count {
                                    let _ = simulate(&EventType::KeyPress(Key::Backspace));
                                    let _ = simulate(&EventType::KeyRelease(Key::Backspace));
                                    thread::sleep(Duration::from_millis(1));
                                }
                                thread::sleep(Duration::from_millis(30));
                                for k in keys_clone {
                                    let _ = simulate(&EventType::KeyPress(k));
                                    let _ = simulate(&EventType::KeyRelease(k));
                                    thread::sleep(Duration::from_millis(1));
                                }
                                let _ = simulate(&EventType::KeyPress(Key::Space));
                                let _ = simulate(&EventType::KeyRelease(Key::Space));
                            });
                        }
                        keys.clear();
                    }
                }
                Key::Backspace => {
                    keys.pop();
                }
                _ => {
                    if key_to_english_char(key).is_some() || key_to_hebrew_char(key).is_some() {
                        keys.push(key);
                    }
                }
            },
            _ => {}
        }
    };

    println!("Listening for keyboard events. Press Space or Enter to check a word.");
    if let Err(err) = listen(callback) {
        eprintln!("Error while listening for keyboard events: {:?}", err);
    }
}
