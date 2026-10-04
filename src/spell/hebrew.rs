//! Conservative, explicitly requested Hebrew spelling offers. No automatic rewrites.

use crate::complete::{PhraseContext, MAX_CANDIDATES};
use crate::config::Config;
use crate::dictionary::{hebrew_stem, Dict, Freq};
use std::collections::HashMap;

/// Search one Unicode-letter edit, including transpositions. Prefix forms reuse
/// the planner's supported prefix stacks and require a ranked dictionary stem.
pub(crate) fn suggestions(
    word: &str,
    dict: Dict,
    freq: Freq,
    context: Option<&PhraseContext>,
) -> Vec<String> {
    let config = Config::global();
    let letters: Vec<char> = word.chars().collect();
    if !config.spell_enabled
        || config.spell_max_dist == 0
        || !(config.spell_min_len.max(3)..=24).contains(&letters.len())
        || !letters.iter().all(|c| ('א'..='ת').contains(c))
        || dict.contains(word)
        || hebrew_stem(word, dict).is_some()
        || crate::complete::ignored(word)
        || crate::complete::learned(word)
        || crate::complete::suppressed(word)
    {
        return Vec::new();
    }
    let mut candidates = HashMap::<String, f32>::new();
    let mut consider = |candidate: Vec<char>, cost: u32| {
        let text: String = candidate.iter().collect();
        if text == word || !valid_finals(&candidate) {
            return;
        }
        let rank = if dict.contains(&text) {
            freq.rank(&text)
        } else {
            hebrew_stem(&text, dict).and_then(|stem| freq.rank(stem))
        };
        let Some(rank) = rank.filter(|rank| *rank <= config.spell_max_rank.min(20_000)) else {
            return;
        };
        let score = super::score(cost, rank, &text)
            - context.map_or(0.0, |context| 20.0 * context.boost(&text).ln() as f32);
        candidates
            .entry(text)
            .and_modify(|old| *old = old.min(score))
            .or_insert(score);
    };
    for index in 0..letters.len() {
        let mut deleted = letters.clone();
        deleted.remove(index);
        consider(deleted, vowel_cost(letters[index]));
        for letter in 'א'..='ת' {
            let mut replaced = letters.clone();
            replaced[index] = letter;
            let cost = if medial(letter) == medial(letters[index]) {
                30
            } else {
                100
            };
            consider(replaced, cost);
        }
        if index + 1 < letters.len() {
            let mut swapped = letters.clone();
            swapped.swap(index, index + 1);
            consider(swapped, 60);
        }
    }
    for index in 0..=letters.len() {
        for letter in 'א'..='ת' {
            let mut inserted = letters.clone();
            inserted.insert(index, letter);
            consider(inserted, vowel_cost(letter));
        }
    }
    let mut candidates: Vec<_> = candidates.into_iter().collect();
    candidates.sort_by(|(a, x), (b, y)| x.total_cmp(y).then_with(|| a.cmp(b)));
    candidates
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|(word, _)| word)
        .collect()
}

fn vowel_cost(letter: char) -> u32 {
    if matches!(letter, 'ו' | 'י') {
        55
    } else {
        100
    }
}

fn medial(letter: char) -> char {
    match letter {
        'ך' => 'כ',
        'ם' => 'מ',
        'ן' => 'נ',
        'ף' => 'פ',
        'ץ' => 'צ',
        _ => letter,
    }
}

fn valid_finals(word: &[char]) -> bool {
    word.iter().enumerate().all(|(index, &letter)| {
        if index + 1 == word.len() {
            !matches!(letter, 'כ' | 'מ' | 'נ' | 'פ' | 'צ')
        } else {
            medial(letter) == letter
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairs_hebrew_letters_and_supported_prefixes() {
        let dict = Dict::of(&["שלום", "מחשב", "סיפור"]);
        let freq = Freq::of(&[("שלום", 10), ("מחשב", 20), ("סיפור", 30)]);
        for (typed, expected) in [
            ("שלומ", "שלום"),
            ("סיפר", "סיפור"),
            ("שלוום", "שלום"),
            ("ספור", "סיפור"),
            ("מחבש", "מחשב"),
            ("במחבש", "במחשב"),
            ("וכשבמחבש", "וכשבמחשב"),
        ] {
            assert_eq!(
                suggestions(typed, dict, freq, None)
                    .first()
                    .map(String::as_str),
                Some(expected),
                "{typed}"
            );
        }
        for typed in ["שלום", "בשלום", "שלוםx", "של", "שלומ!", "של1מ"] {
            assert!(suggestions(typed, dict, freq, None).is_empty(), "{typed}");
        }
        assert!(suggestions("שלומ", dict, Freq::EMPTY, None).is_empty());
    }

    #[test]
    fn final_letters_are_only_offered_at_word_end() {
        assert!(valid_finals(&['ש', 'ל', 'ו', 'ם']));
        assert!(!valid_finals(&['ש', 'ם', 'ו', 'ם']));
        assert!(!valid_finals(&['ש', 'ל', 'ו', 'מ']));
    }

    #[test]
    fn completion_request_exposes_hebrew_spelling_offers() {
        let keys: Vec<char> = "שלומ".chars().collect();
        let offers = crate::dictionary::complete_candidates(
            &keys,
            Some,
            |_| false,
            Dict::of(&["שלום"]),
            Some(crate::types::Language::Hebrew),
            None,
        );
        assert_eq!(offers.first().map(String::as_str), Some("שלום"));
    }
}
