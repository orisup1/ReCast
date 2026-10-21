//! Offline correction preview. Only the supplied text is inspected.

use crate::complete::PhraseContext;
use crate::dictionary::{self, Fix, History, Run};
use crate::types::Language;

const MAX_CONTEXT_WORDS: usize = 16;

pub fn run(args: &[String]) -> Result<(), String> {
    let mut word = None;
    let mut layout = None;
    let mut context = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--explain" if word.is_none() => {
                word = Some(
                    args.next()
                        .filter(|value| !value.starts_with("--"))
                        .ok_or("--explain requires a word")?,
                );
            }
            "--layout" if layout.is_none() => {
                layout = Some(
                    args.next()
                        .filter(|value| !value.starts_with("--"))
                        .ok_or("--layout requires en|he")?,
                );
            }
            "--context" if context.is_none() => {
                context = Some(
                    args.next()
                        .filter(|value| !value.starts_with("--"))
                        .ok_or("--context requires preceding visible words")?,
                );
            }
            _ => return Err(format!("Unexpected preview option: {arg}")),
        }
    }
    let word = word
        .ok_or("Usage: recast --explain WORD --layout en|he [--context 'prior visible words']")?;
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
    if context.is_some_and(|text| text.split_whitespace().count() > MAX_CONTEXT_WORDS) {
        return Err(format!(
            "--context accepts at most {MAX_CONTEXT_WORDS} preceding words"
        ));
    }
    let keys: Vec<_> = word
        .chars()
        .map(|c| {
            readings_for_layout(c, current)
                .ok_or_else(|| format!("Unsupported character {c:?} for this layout"))
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
    let (phrase, run) = context.map_or_else(
        || (PhraseContext::default(), Run::default()),
        |text| preceding_context(text),
    );
    let result = dictionary::check_and_correct_in_context(
        &keys,
        |k| Some(k.0),
        |k| Some(k.1),
        |k| k.2,
        run,
        dictionary::en_dict(),
        dictionary::he_dict(),
        Some(current),
        false,
        context.map(|_| &phrase),
        |_| crate::layout::LayoutSwitch::Switched,
    );
    let after = match result.fix {
        None => word.clone(),
        Some(Fix::Layout { start, text, .. }) => {
            word.chars().take(start).collect::<String>() + &text
        }
        Some(
            Fix::Spelling { text, .. }
            | Fix::LayoutSpelling { text, .. }
            | Fix::Suffix { text, .. },
        ) => text,
    };
    println!(
        "Input: {word:?}\nLayout: {}\nReplacement: {after:?}\nReason: {}",
        layout.unwrap(),
        result.reason
    );
    if let Some(context) = context {
        println!("Context: {context:?}");
    }
    println!("Details:\n{}", result.details);
    if context.is_some() {
        println!("Preview uses this invocation's settings, saved lists, and supplied prior words, without live app checks.");
    } else {
        println!("Preview uses this invocation's settings and saved lists, without prior words or live app checks.");
    }
    println!(
        "No text is injected or learned; supplied context stays in memory for this invocation."
    );
    Ok(())
}

fn preceding_context(text: &str) -> (PhraseContext, Run) {
    let mut phrase = PhraseContext::default();
    let mut history = History::default();
    for part in text.split_inclusive(char::is_whitespace) {
        let word = part.trim_end_matches(char::is_whitespace);
        if !word.is_empty() {
            phrase.observe(word, false);
            let word = word.to_lowercase();
            let language = if word.chars().all(|c| c.is_ascii_lowercase())
                && dictionary::en_dict().contains(&word)
            {
                Some(Language::English)
            } else if word.chars().all(|c| ('א'..='ת').contains(&c))
                && dictionary::he_dict().contains(&word)
            {
                Some(Language::Hebrew)
            } else {
                None
            };
            if let Some(language) = language {
                history.push(language);
            } else {
                history.clear();
            }
        }
        if part.contains(['\r', '\n', '\u{0085}', '\u{2028}', '\u{2029}']) {
            phrase.clear();
            history.clear();
        }
    }
    (phrase, history.run())
}

fn readings_for_layout(c: char, current: Language) -> Option<(char, char, bool)> {
    match current {
        Language::English => readings(c),
        // Prefer unshifted keys for letters, while allowing shifted symbols.
        Language::Hebrew => (' '..='~')
            .filter(|c| !c.is_ascii_uppercase())
            .filter_map(readings)
            .filter(|key| key.1 == c)
            .min_by_key(|key| key.2),
    }
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
    let hebrew = crate::keymap::hebrew_char_shifted(Some(c), Some(hebrew), shift)?;
    Some((c.to_ascii_lowercase(), hebrew, shift))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifted_hebrew_preview_symbols_keep_their_actual_readings() {
        for c in "~!@#$%^&*()_+{}|:\"<>?".chars() {
            let key = readings_for_layout(c, Language::Hebrew).unwrap();
            assert_eq!(key.1, c);
            assert!(key.2, "{c:?}");
        }
        for c in "שלום0123456789[]\\/.,;'".chars() {
            let key = readings_for_layout(c, Language::Hebrew).unwrap();
            assert_eq!(key.1, c);
            assert!(!key.2, "{c:?}");
        }
        assert_eq!(
            readings_for_layout('(', Language::Hebrew),
            Some((')', '(', true))
        );
        assert_eq!(
            readings_for_layout('A', Language::English),
            Some(('a', 'ש', true))
        );
        assert!(readings_for_layout('🙂', Language::Hebrew).is_none());
    }

    #[test]
    fn shifted_punctuation_preview_preserves_corrections_and_wrappers() {
        for (word, current, expected) in [
            ("akuo!", Language::English, "שלום!"),
            ("שלום!", Language::Hebrew, "שלום!"),
            ("תודדה?", Language::Hebrew, "תודה?"),
            ("(תודדה!)", Language::Hebrew, "(תודה!)"),
            ("{רקבןקהק?}", Language::Hebrew, "{receive?}"),
        ] {
            let keys: Vec<_> = word
                .chars()
                .map(|c| readings_for_layout(c, current).unwrap())
                .collect();
            let result = dictionary::check_and_correct_in_context(
                &keys,
                |key| Some(key.0),
                |key| Some(key.1),
                |key| key.2,
                Run::default(),
                dictionary::en_dict(),
                dictionary::he_dict(),
                Some(current),
                false,
                None,
                |_| crate::layout::LayoutSwitch::Switched,
            );
            let after = match result.fix {
                None => word.to_owned(),
                Some(Fix::Layout { start, text, .. }) => {
                    word.chars().take(start).collect::<String>() + &text
                }
                Some(Fix::Spelling { text, .. } | Fix::LayoutSpelling { text, .. }) => text,
                other => panic!("{word:?}: {other:?}"),
            };
            assert_eq!(after, expected, "{word:?}; {}", result.details);
        }
    }
}
