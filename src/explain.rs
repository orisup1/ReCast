//! Offline correction preview. Only the supplied text is inspected.

use crate::dictionary::{self, Fix, Run};
use crate::types::Language;

pub fn run(args: &[String]) -> Result<(), String> {
    let mut word = None;
    let mut layout = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--explain" if word.is_none() => word = args.next(),
            "--layout" if layout.is_none() => layout = args.next(),
            _ => return Err(format!("Unexpected preview option: {arg}")),
        }
    }
    let word = word.ok_or("Usage: recast --explain WORD --layout en|he")?;
    let current = match layout.map(String::as_str) {
        Some("en") => Language::English,
        Some("he") => Language::Hebrew,
        _ => return Err("--explain requires --layout en|he".into()),
    };
    if word.is_empty() || word.chars().any(char::is_whitespace) {
        return Err("Supply one non-empty word, optionally with trailing punctuation".into());
    }
    if word.chars().count() > crate::types::MAX_WORD_KEYS {
        return Err(format!(
            "Live correction skips words longer than {} keys",
            crate::types::MAX_WORD_KEYS
        ));
    }
    let keys: Vec<_> = word
        .chars()
        .map(|c| {
            let key = match current {
                Language::English => readings(c),
                // Search unshifted US keys first: Hebrew letters have no case.
                Language::Hebrew => (' '..='~')
                    .filter(|c| !c.is_ascii_uppercase())
                    .filter_map(readings)
                    .find(|key| key.1 == c && !key.2),
            };
            key.ok_or_else(|| format!("Unsupported character {c:?} for this layout"))
        })
        .collect::<Result<_, _>>()?;
    crate::require_readable_config();
    crate::config::Config::update_live(|cfg| *cfg = crate::config::Config::from_env());
    for complaint in crate::settings::complaints(
        crate::config::NUMERIC_KEYS,
        crate::config::BOOLEAN_KEYS,
        crate::config::ALL_KEYS,
    ) {
        eprintln!("{complaint}");
    }
    let result = dictionary::check_and_correct(
        &keys,
        |k| Some(k.0),
        |k| Some(k.1),
        |k| k.2,
        Run::default(),
        dictionary::en_dict(),
        dictionary::he_dict(),
        Some(current),
        false,
        |_| crate::layout::LayoutSwitch::Switched,
    );
    let after = match result.fix {
        None => word.clone(),
        Some(Fix::Layout { start, text, .. }) => {
            word.chars().take(start).collect::<String>() + &text
        }
        Some(Fix::Spelling { text } | Fix::LayoutSpelling { text, .. }) => text,
    };
    println!(
        "Input: {word:?}\nLayout: {}\nReplacement: {after:?}\nReason: {}",
        layout.unwrap(),
        result.reason
    );
    println!("Preview uses this invocation's settings and saved lists, without prior words or live app checks.");
    Ok(())
}

fn readings(c: char) -> Option<(char, char, bool)> {
    #[cfg(target_os = "linux")]
    let (hebrew, shift) = {
        let (key, shift) = crate::keymap::english_char_to_evkey_shifted(c)?;
        (crate::keymap::evkey_to_hebrew_char(key)?, shift)
    };
    #[cfg(not(target_os = "linux"))]
    let (hebrew, shift) = {
        let (key, shift) = crate::keymap::english_char_to_key(c)?;
        (crate::keymap::key_to_hebrew_char(key)?, shift)
    };
    Some((c.to_ascii_lowercase(), hebrew, shift))
}
