use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufRead};

use crate::layout::switch_layout_to;
use crate::types::Language;

/// Load a plain-text word list (one word per line) into a `HashSet`.
pub fn load_dictionary(path: &str) -> io::Result<HashSet<String>> {
    let file = File::open(path)?;
    let reader = io::BufReader::new(file);
    let mut dict = HashSet::new();
    for line in reader.lines() {
        let word = line?.trim().to_lowercase();
        if !word.is_empty() {
            dict.insert(word);
        }
    }
    Ok(dict)
}

/// Given the same physical key sequence interpreted as English (`word_en`) and
/// Hebrew (`word_he`), decide whether the layout must be switched and do it.
/// Returns `true` when a switch was performed.
pub fn check_and_switch_candidates(
    word_en: &str,
    word_he: &str,
    en_dict: &HashSet<String>,
    he_dict: &HashSet<String>,
) -> bool {
    let word_en_lower = word_en.to_lowercase();
    let word_he_lower = word_he.to_lowercase();
    let is_in_en = !word_en_lower.is_empty() && en_dict.contains(&word_en_lower);
    let is_in_he = !word_he_lower.is_empty() && he_dict.contains(&word_he_lower);

    let final_en = is_in_en && !is_in_he;
    let final_he = is_in_he && !is_in_en;

    let target_lang = if final_en {
        Some(Language::English)
    } else if final_he {
        Some(Language::Hebrew)
    } else {
        None
    };

    println!("{}", word_en);
    println!("English: {}", final_en);
    println!("Hebrew: {}", final_he);

    if let Some(lang) = target_lang {
        let switched = switch_layout_to(lang);
        println!("switching: {}", if switched { "True" } else { "False" });
        switched
    } else {
        println!("switching: False");
        false
    }
}
