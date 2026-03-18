mod dictionary;
mod keymap;
mod layout;
mod platform;
mod types;

use std::collections::HashSet;

use dictionary::load_dictionary;

fn main() {
    let en_dict = load_dictionary("en_dict.txt").unwrap_or_else(|e| {
        eprintln!("Warning: Could not load English dictionary: {}", e);
        HashSet::new()
    });
    let he_dict = load_dictionary("he_dict.txt").unwrap_or_else(|e| {
        eprintln!("Warning: Could not load Hebrew dictionary: {}", e);
        HashSet::new()
    });

    if en_dict.is_empty() || he_dict.is_empty() {
        eprintln!("Fatal: At least one dictionary must be loaded. Shutting down.");
        return;
    }

    #[cfg(target_os = "linux")]
    platform::linux::run(en_dict, he_dict);

    #[cfg(target_os = "macos")]
    platform::macos::run(en_dict, he_dict);
}
