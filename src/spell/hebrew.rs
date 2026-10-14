//! Conservative Hebrew spelling with stricter confidence for automatic rewrites.

use crate::complete::{PhraseContext, MAX_CANDIDATES};
use crate::config::Config;
use crate::dictionary::{hebrew_stem, Dict, Freq};
use std::collections::HashMap;
use std::sync::OnceLock;

enum Request {
    Automatic,
    Completion,
    Correction,
}

/// Search one Unicode-letter edit, including transpositions. Prefix forms reuse
/// the planner's supported prefix stacks and require a ranked dictionary stem.
pub(crate) fn suggestions(
    word: &str,
    dict: Dict,
    freq: Freq,
    context: Option<&PhraseContext>,
) -> Vec<String> {
    ranked_candidates(
        word,
        dict,
        freq,
        context,
        Request::Completion,
        &Config::global(),
    )
    .into_iter()
    .take(MAX_CANDIDATES)
    .map(|(word, _)| word)
    .collect()
}

/// Explicit correction reconsiders real and short words independently of
/// automatic spelling settings, while keeping the existing one-edit search.
pub(crate) fn correction_suggestions(
    word: &str,
    dict: Dict,
    freq: Freq,
    context: Option<&PhraseContext>,
) -> Vec<String> {
    ranked_candidates(
        word,
        dict,
        freq,
        context,
        Request::Correction,
        &Config::global(),
    )
    .into_iter()
    .take(MAX_CANDIDATES)
    .map(|(word, _)| word)
    .collect()
}

/// Automatic repairs require a clear score lead; ambiguous offers remain manual.
/// Bound phrase influence so context cannot manufacture a confident correction.
pub(crate) fn correct(
    word: &str,
    dict: Dict,
    freq: Freq,
    context: Option<&PhraseContext>,
) -> Option<String> {
    let mut candidates = ranked_candidates(
        word,
        dict,
        freq,
        context,
        Request::Automatic,
        &Config::global(),
    )
    .into_iter();
    let (best, score) = candidates.next()?;
    if candidates
        .next()
        .is_some_and(|(_, runner_up)| runner_up - score < 20.0)
    {
        return None;
    }
    Some(best)
}

fn ranked_candidates(
    word: &str,
    dict: Dict,
    freq: Freq,
    context: Option<&PhraseContext>,
    request: Request,
    config: &Config,
) -> Vec<(String, f32)> {
    let automatic = matches!(request, Request::Automatic);
    let explicit = matches!(request, Request::Correction);
    let Some(letters) = spelling_letters(word, !automatic) else {
        return Vec::new();
    };
    let plain: String = letters.iter().map(|(letter, _)| letter).collect();
    let min_len = if explicit {
        2
    } else {
        config.spell_min_len.max(if automatic { 4 } else { 3 })
    };
    let max_rank = if explicit {
        50_000
    } else {
        config.spell_max_rank.min(20_000)
    };
    if (!explicit && (!config.spell_enabled || config.spell_max_dist == 0))
        || !(min_len..=24).contains(&letters.len())
        || (!explicit
            && (dict.contains(word)
                || dict.contains(&plain)
                || hebrew_stem(&plain, dict).is_some()
                || crate::complete::ignored(word)
                || crate::complete::learned(word)
                || crate::complete::suppressed(word)))
    {
        return Vec::new();
    }
    let mut candidates = HashMap::<String, f32>::new();
    let mut consider = |candidate: Vec<(char, String)>, cost: u32| {
        let chars: Vec<char> = candidate.iter().map(|(letter, _)| *letter).collect();
        let text: String = chars.iter().collect();
        if text == plain || !valid_finals(&chars) {
            return;
        }
        let rank = if dict.contains(&text) {
            freq.rank(&text)
        } else {
            hebrew_stem(&text, dict).and_then(|stem| freq.rank(stem))
        };
        let Some(rank) = rank.filter(|rank| *rank <= max_rank) else {
            return;
        };
        let discount = context.map_or(0.0, |context| 20.0 * context.boost(&text).ln() as f32);
        let score = super::score(cost, rank, &text)
            - if automatic {
                discount.min(super::MAX_AUTOMATIC_CONTEXT_DISCOUNT)
            } else {
                discount
            };
        let visible: String = candidate
            .into_iter()
            .map(|(letter, marks)| format!("{letter}{marks}"))
            .collect();
        candidates
            .entry(visible)
            .and_modify(|old| *old = old.min(score))
            .or_insert(score);
    };
    for index in 0..letters.len() {
        // Never delete an attached mark or quote along with its letter.
        if letters[index].1.is_empty() {
            let mut deleted = letters.clone();
            deleted.remove(index);
            consider(deleted, vowel_cost(letters[index].0));
        }
        for letter in 'א'..='ת' {
            let mut replaced = letters.clone();
            replaced[index].0 = letter;
            // Neighbor weighting starts in manual offers; automatic costs stay calibrated.
            let cost = if automatic && medial(letters[index].0) != medial(letter) {
                100
            } else {
                substitution_cost(letters[index].0, letter)
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
            inserted.insert(index, (letter, String::new()));
            consider(inserted, vowel_cost(letter));
        }
    }
    let mut candidates: Vec<_> = candidates.into_iter().collect();
    candidates.sort_by(|(a, x), (b, y)| x.total_cmp(y).then_with(|| a.cmp(b)));
    candidates
}

pub(crate) fn is_mark(letter: char) -> bool {
    matches!(letter, '\u{0591}'..='\u{05bd}' | '\u{05bf}' | '\u{05c1}'..='\u{05c2}' | '\u{05c4}'..='\u{05c5}' | '\u{05c7}')
}

fn spelling_letters(word: &str, decorated: bool) -> Option<Vec<(char, String)>> {
    let mut letters: Vec<(char, String)> = Vec::new();
    for letter in word.chars() {
        if ('א'..='ת').contains(&letter) {
            letters.push((letter, String::new()));
        } else if decorated
            && (is_mark(letter) || matches!(letter, '\u{05f3}' | '\u{05f4}' | '\'' | '"'))
        {
            letters.last_mut()?.1.push(letter);
        } else {
            return None;
        }
    }
    Some(letters)
}

fn substitution_cost(a: char, b: char) -> u32 {
    if medial(a) == medial(b) {
        return 30;
    }
    static POSITIONS: OnceLock<[Option<(i32, i32)>; 27]> = OnceLock::new();
    let positions = POSITIONS.get_or_init(|| {
        std::array::from_fn(|index| {
            let letter = char::from_u32('א' as u32 + index as u32).unwrap();
            let key = crate::keymap::convert_selection(&letter.to_string());
            super::qwerty_pos(key.as_bytes()[0])
        })
    });
    if let (Some((row_a, col_a)), Some((row_b, col_b))) = (
        positions[(a as u32 - 'א' as u32) as usize],
        positions[(b as u32 - 'א' as u32) as usize],
    ) {
        if (row_a - row_b).abs() <= 1 && (col_a - col_b).abs() <= 2 {
            return if row_a == row_b {
                super::COST_ADJACENT_ROW
            } else {
                super::COST_ADJACENT_DIAG
            };
        }
    }
    100
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
    fn keyboard_neighbors_rank_ahead_of_distant_substitutions() {
        assert_eq!(substitution_cost('מ', 'ם'), 30);
        assert_eq!(substitution_cost('פ', 'ם'), super::super::COST_ADJACENT_ROW);
        assert_eq!(substitution_cost('ת', 'ץ'), super::super::COST_ADJACENT_ROW);
        assert_eq!(
            substitution_cost('ש', 'ז'),
            super::super::COST_ADJACENT_DIAG
        );
        assert_eq!(substitution_cost('פ', 'ש'), 100);
        let dict = Dict::of(&["שלום", "שלוש"]);
        let freq = Freq::of(&[("שלום", 100), ("שלוש", 100)]);
        assert_eq!(correction_suggestions("שלופ", dict, freq, None)[0], "שלום");
    }

    #[test]
    fn manual_repairs_keep_attached_marks_and_quotes() {
        let dict = Dict::of(&["שלום", "מחשב"]);
        let freq = Freq::of(&[("שלום", 10), ("מחשב", 20)]);
        for (typed, expected) in [
            ("שָׁלוֹמ", "שָׁלוֹם"),
            ("שלו״מ", "שלו״ם"),
            ("שלו׳מ", "שלו׳ם"),
            ("שלו'מ", "שלו'ם"),
            ("שלו\"מ", "שלו\"ם"),
            ("שָׁלוֹום", "שָׁלוֹם"),
            ("מחַבש", "מחַשב"),
        ] {
            assert_eq!(
                correction_suggestions(typed, dict, freq, None)[0],
                expected,
                "{typed}"
            );
            assert_eq!(correct(typed, dict, freq, None), None, "{typed}");
        }
        assert!(suggestions("שָׁלוֹם", dict, freq, None).is_empty());
        assert!(correction_suggestions("שָׁלוֹםזַ", dict, freq, None).is_empty());
        for malformed in ["ָשלום", "׳שלום", "שלום-ם", "שלום1", "שלוםx"] {
            assert!(correction_suggestions(malformed, dict, freq, None).is_empty());
        }
        let keys: Vec<_> = "שָׁלוֹמַ!".chars().collect();
        let offers = crate::dictionary::correction_candidates(
            &keys,
            Some,
            |_| false,
            dict,
            Some(crate::types::Language::Hebrew),
            None,
        );
        assert_eq!(offers[0], "שָׁלוֹםַ!");
    }

    #[test]
    fn explicit_correction_ignores_automatic_spelling_settings() {
        let dict = Dict::of(&["שלום", "שלוש", "שלם"]);
        let freq = Freq::of(&[("שלום", 100), ("שלוש", 100), ("שלם", 30_000)]);
        let mut config = Config::global();
        config.spell_enabled = false;
        config.spell_min_len = 10;
        config.spell_max_dist = 0;
        config.spell_max_rank = 0;
        for (typed, expected) in [("שלומ", "שלום"), ("שלום", "שלוש"), ("של", "שלם")]
        {
            let offers = ranked_candidates(typed, dict, freq, None, Request::Correction, &config);
            assert!(
                offers.iter().any(|(word, _)| word == expected),
                "{typed}: {offers:?}"
            );
            assert!(!offers.iter().any(|(word, _)| word == typed));
            for request in [Request::Automatic, Request::Completion] {
                assert!(ranked_candidates(typed, dict, freq, None, request, &config).is_empty());
            }
        }
        for typed in ["ש", "שלומ!", "של1מ", "http://שלומ", "שלוםx"] {
            assert!(
                ranked_candidates(typed, dict, freq, None, Request::Correction, &config).is_empty()
            );
        }
        let offers = correction_suggestions("שלום", dict, freq, None);
        assert!(!offers.is_empty());
        assert!(!offers.iter().any(|word| word == "שלום"));
        assert!(offers.len() <= MAX_CANDIDATES);
        assert_eq!(offers, correction_suggestions("שלום", dict, freq, None));
    }

    #[test]
    fn automatic_repairs_require_a_clear_winner() {
        let dict = Dict::of(&["שלום", "מחשב", "סיפור"]);
        let freq = Freq::of(&[("שלום", 10), ("מחשב", 20), ("סיפור", 30)]);
        for (typed, expected) in [("שלומ", "שלום"), ("מחבש", "מחשב"), ("ספור", "סיפור")]
        {
            assert_eq!(correct(typed, dict, freq, None).as_deref(), Some(expected));
        }
        for typed in ["שלום", "בשלום", "של", "שלומ!", "של1מ"] {
            assert_eq!(correct(typed, dict, freq, None), None, "{typed}");
        }
        let tied = Dict::of(&["שלום", "שלוש"]);
        let ranks = Freq::of(&[("שלום", 100), ("שלוש", 100)]);
        assert_eq!(suggestions("שלוב", tied, ranks, None).len(), 2);
        assert_eq!(correct("שלוב", tied, ranks, None), None);
        assert_eq!(correct("שלומ", dict, Freq::EMPTY, None), None);
    }

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
