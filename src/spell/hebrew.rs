//! Conservative Hebrew spelling with stricter confidence for automatic rewrites.

use crate::complete::{PhraseContext, MAX_CANDIDATES};
use crate::config::Config;
use crate::dictionary::{hebrew_stem, ranked_hebrew_stem, Dict, Freq};
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Copy)]
enum Request {
    Automatic,
    Completion,
    Correction,
}

const MIN_SCORE_GAP: f32 = 20.0;
const PREFIX_INFERENCE_COST: u32 = 12;

struct RankedCandidate {
    evidence: super::Candidate,
    stem: Option<String>,
    context_discount: f32,
    learned_discount: f32,
}

#[derive(Default)]
struct SearchResult {
    candidates: Vec<RankedCandidate>,
    rejected_rank: Vec<(String, u32, u32)>,
    protection: Option<&'static str>,
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
#[cfg(test)]
pub(crate) fn correct(
    word: &str,
    dict: Dict,
    freq: Freq,
    context: Option<&PhraseContext>,
) -> Option<String> {
    assess(word, dict, freq, context).correction
}

pub(crate) fn assess(
    word: &str,
    dict: Dict,
    freq: Freq,
    context: Option<&PhraseContext>,
) -> super::Assessment {
    assess_with(word, dict, freq, context, &Config::global())
}

fn assess_with(
    word: &str,
    dict: Dict,
    freq: Freq,
    context: Option<&PhraseContext>,
    config: &Config,
) -> super::Assessment {
    let result = search(word, dict, freq, context, Request::Automatic, config);
    if let Some(reason) = result.protection {
        return super::Assessment::unchanged(reason);
    }
    let mut details = format!(
        "Hebrew spelling candidates (lower score is better; scores are not confidence percentages):\nLimits: one letter edit, configured maximum frequency rank {}; edits without a keyboard or repetition explanation are limited to rank 20000.", config.spell_max_rank
    );
    for (text, rank, cost) in &result.rejected_rank {
        details.push_str(&format!(
            "\nRejected by frequency-rank gate: {text}: frequency rank {rank}, edit cost {cost}."
        ));
    }
    let Some(best) = result.candidates.first() else {
        details.push_str(
            "\nNo Hebrew candidate passed the dictionary, final-letter, and frequency-rank gates.",
        );
        return super::Assessment::unchanged(&details);
    };
    for candidate in result.candidates.iter().take(2) {
        let evidence = &candidate.evidence;
        details.push_str(&format!(
            "\n  {}: score {:.2}, edit cost {}, frequency rank {}; context discount {:.2}; learned edit discount {:.2}.",
            evidence.text, evidence.score, evidence.cost, evidence.rank, candidate.context_discount, candidate.learned_discount
        ));
        if let Some(stem) = &candidate.stem {
            details.push_str(&format!(
                "\n  Prefix inference reused ranked dictionary stem {stem:?}; cost surcharge {PREFIX_INFERENCE_COST}."
            ));
        }
    }
    let clear_gap = result
        .candidates
        .get(1)
        .is_none_or(|second| second.evidence.score - best.evidence.score >= MIN_SCORE_GAP);
    if let Some(second) = result.candidates.get(1) {
        details.push_str(&format!(
            "\nScore gap: {:.2}; required minimum: {MIN_SCORE_GAP:.2}.",
            second.evidence.score - best.evidence.score
        ));
    }
    // An unranked but valid word beneath a repeated key is another plausible
    // original. Do not let a frequent neighbor substitute for that uncertainty.
    let keep_cost = if unranked_repeat_base(word, dict, freq) {
        super::COST_DOUBLE
    } else {
        super::COST_EDIT
    };
    let keep_score = super::score(keep_cost, config.spell_max_rank, word);
    let beats_original = keep_score - best.evidence.score >= MIN_SCORE_GAP;
    details.push_str(&format!(
        "\nKeep original: score {keep_score:.2}; replacement lead {:.2}; required minimum: {MIN_SCORE_GAP:.2}.", keep_score - best.evidence.score
    ));
    let accepted = clear_gap && beats_original;
    details.push_str(if accepted {
        "\nAccepted the best Hebrew spelling candidate."
    } else if !clear_gap {
        "\nLeft unchanged: the leading Hebrew candidates are too close in score."
    } else {
        "\nLeft unchanged: the Hebrew candidate did not overcome keeping the original token."
    });
    super::Assessment {
        correction: accepted.then(|| best.evidence.text.clone()),
        details,
        best: Some(best.evidence.clone()),
    }
}

fn unranked_repeat_base(word: &str, dict: Dict, freq: Freq) -> bool {
    let letters: Vec<_> = word.chars().collect();
    letters.windows(2).enumerate().any(|(index, pair)| {
        if pair[0] != pair[1] {
            return false;
        }
        let base: String = letters[..index]
            .iter()
            .chain(&letters[index + 1..])
            .collect();
        (dict.contains(&base) || hebrew_stem(&base, dict).is_some())
            && freq.rank(&base).is_none()
            && ranked_hebrew_stem(&base, dict, freq, 1).is_none()
    })
}

fn ranked_candidates(
    word: &str,
    dict: Dict,
    freq: Freq,
    context: Option<&PhraseContext>,
    request: Request,
    config: &Config,
) -> Vec<(String, f32)> {
    search(word, dict, freq, context, request, config)
        .candidates
        .into_iter()
        .map(|candidate| (candidate.evidence.text, candidate.evidence.score))
        .collect()
}

fn search(
    word: &str,
    dict: Dict,
    freq: Freq,
    context: Option<&PhraseContext>,
    request: Request,
    config: &Config,
) -> SearchResult {
    let mut result = SearchResult::default();
    let automatic = matches!(request, Request::Automatic);
    let explicit = matches!(request, Request::Correction);
    let Some(letters) = spelling_letters(word, !automatic) else {
        result.protection = Some("Hebrew spelling requires Hebrew letters; automatic repairs protect attached marks, quotes, and internal punctuation.");
        return result;
    };
    let plain: String = letters.iter().map(|(letter, _)| letter).collect();
    let min_len = if explicit {
        2
    } else {
        config.spell_min_len.max(if automatic { 4 } else { 3 })
    };
    if !explicit && (!config.spell_enabled || config.spell_max_dist == 0) {
        result.protection =
            Some("Hebrew spelling is disabled by the spelling setting or edit-distance limit.");
        return result;
    }
    if !(min_len..=24).contains(&letters.len()) {
        result.protection = Some("The word is outside the configured Hebrew spelling length limits (at least four letters for automatic repairs, at most 24).");
        return result;
    }
    if !explicit
        && (dict.contains(word) || dict.contains(&plain) || hebrew_stem(&plain, dict).is_some())
    {
        result.protection = Some("Protected Hebrew dictionary word or recognized prefix form.");
        return result;
    }
    if !explicit
        && (crate::complete::ignored(word)
            || crate::complete::learned(word)
            || crate::complete::suppressed(word))
    {
        result.protection = Some("Protected Hebrew word: saved word rule or undo exception.");
        return result;
    }
    let mut candidates = HashMap::<String, RankedCandidate>::new();
    let typo_counts = crate::personal::hebrew_typo_counts();
    let mut consider = |candidate: Vec<(char, String)>, cost: u32| {
        let chars: Vec<char> = candidate.iter().map(|(letter, _)| *letter).collect();
        let text: String = chars.iter().collect();
        if text == plain || !valid_finals(&chars) {
            return;
        }
        let direct_rank = dict.contains(&text).then(|| freq.rank(&text)).flatten();
        let stem = if direct_rank.is_some() {
            None
        } else {
            ranked_hebrew_stem(&text, dict, freq, 1)
        };
        let Some(rank) = direct_rank.or_else(|| stem.map(|(_, rank)| rank)) else {
            return;
        };
        let max_rank = if explicit {
            50_000
        } else if automatic && cost <= super::COST_ADJACENT_DIAG {
            config.spell_max_rank
        } else {
            config.spell_max_rank.min(20_000)
        };
        if rank > max_rank {
            if !result.rejected_rank.iter().any(|(word, ..)| word == &text) {
                result.rejected_rank.push((text, rank, cost));
                result
                    .rejected_rank
                    .sort_by(|a, b| (a.1, a.2, &a.0).cmp(&(b.1, b.2, &b.0)));
                result.rejected_rank.truncate(3);
            }
            return;
        }
        let discount = context.map_or(0.0, |context| 20.0 * context.boost(&text).ln() as f32);
        let discount = if automatic {
            discount.min(super::MAX_AUTOMATIC_CONTEXT_DISCOUNT)
        } else {
            discount
        };
        let cost = cost
            + if stem.is_some() {
                PREFIX_INFERENCE_COST
            } else {
                0
            };
        let learned_discount = super::class_discount(typo_class(&plain, &text), typo_counts);
        let score = super::score(cost, rank, &text) - discount - learned_discount;
        let visible: String = candidate
            .into_iter()
            .map(|(letter, marks)| format!("{letter}{marks}"))
            .collect();
        let candidate = RankedCandidate {
            evidence: super::Candidate {
                text: visible.clone(),
                score,
                cost,
                rank,
            },
            stem: stem.map(|(stem, _)| stem.to_owned()),
            context_discount: discount,
            learned_discount,
        };
        if candidates
            .get(&visible)
            .is_none_or(|old| score < old.evidence.score)
        {
            candidates.insert(visible, candidate);
        }
    };
    for index in 0..letters.len() {
        // Never delete an attached mark or quote along with its letter.
        if letters[index].1.is_empty() {
            let mut deleted = letters.clone();
            deleted.remove(index);
            consider(deleted, deletion_cost(&letters, index));
        }
        for letter in 'א'..='ת' {
            let mut replaced = letters.clone();
            replaced[index].0 = letter;
            consider(replaced, substitution_cost(letters[index].0, letter));
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
            let repeated = index.checked_sub(1).is_some_and(|i| letters[i].0 == letter)
                || letters.get(index).is_some_and(|next| next.0 == letter);
            consider(
                inserted,
                if repeated {
                    super::COST_DOUBLE
                } else {
                    vowel_cost(letter)
                },
            );
        }
    }
    result.candidates = candidates.into_values().collect();
    result.candidates.sort_by(|a, b| {
        a.evidence
            .score
            .total_cmp(&b.evidence.score)
            .then_with(|| a.evidence.text.cmp(&b.evidence.text))
    });
    result
}

fn deletion_cost(letters: &[(char, String)], index: usize) -> u32 {
    let letter = letters[index].0;
    let neighbors = index
        .checked_sub(1)
        .into_iter()
        .chain((index + 1 < letters.len()).then_some(index + 1));
    if neighbors.clone().any(|i| letters[i].0 == letter) {
        super::COST_DOUBLE
    } else if neighbors
        .into_iter()
        .any(|i| substitution_cost(letter, letters[i].0) <= super::COST_ADJACENT_DIAG)
    {
        vowel_cost(letter).min(super::COST_STRAY_KEY)
    } else {
        vowel_cost(letter)
    }
}

/// Only exact one-edit pairs train the channel. Marks, mixed scripts, and
/// multi-edit corrections cannot supply reliable keyboard-class evidence.
pub(crate) fn typo_class(typed: &str, corrected: &str) -> Option<usize> {
    if typed.is_empty()
        || corrected.is_empty()
        || !typed
            .chars()
            .chain(corrected.chars())
            .all(|c| ('א'..='ת').contains(&c))
    {
        return None;
    }
    let (a, b): (Vec<_>, Vec<_>) = (typed.chars().collect(), corrected.chars().collect());
    let prefix = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
    let (a, b) = (&a[prefix..], &b[prefix..]);
    if a.len() == b.len() {
        if a.len() >= 2 && a[0] == b[1] && a[1] == b[0] && a[2..] == b[2..] {
            return Some(0);
        }
        if !a.is_empty()
            && a[1..] == b[1..]
            && medial(a[0]) != medial(b[0])
            && substitution_cost(a[0], b[0]) <= super::COST_ADJACENT_DIAG
        {
            return Some(1);
        }
    } else if (a.len() == b.len() + 1 && a[1..] == *b) || (b.len() == a.len() + 1 && *a == b[1..]) {
        return Some(2);
    }
    None
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
    fn cheap_hebrew_repairs_use_the_configured_rank_ceiling() {
        let mut config = Config::global();
        config.spell_enabled = true;
        config.spell_min_len = 4;
        config.spell_max_dist = 1;
        config.spell_max_rank = 40_000;
        let d = Dict::of(&["מסמך"]);
        let f = Freq::of(&[("מסמך", 35_000)]);
        let repaired = assess_with("מסמךך", d, f, None, &config);
        assert_eq!(
            repaired.correction.as_deref(),
            Some("מסמך"),
            "{}",
            repaired.details
        );
        assert!(repaired.details.contains("edit cost 55"));
        let rejected = assess_with("מסמז", d, f, None, &config);
        assert_eq!(
            rejected.correction, None,
            "an arbitrary substitution retains the tighter gate"
        );
        assert!(rejected
            .details
            .contains("Rejected by frequency-rank gate: מסמך"));
        assert_eq!(
            assess_with(
                "שלופ",
                Dict::of(&["שלום"]),
                Freq::of(&[("שלום", 30_000)]),
                None,
                &config
            )
            .correction
            .as_deref(),
            Some("שלום")
        );
        config.spell_max_rank = 30_000;
        let rejected = assess_with("מסמךך", d, f, None, &config);
        assert_eq!(rejected.correction, None);
        assert!(rejected.details.contains("frequency rank 35000"));
        config.spell_max_dist = 0;
        assert!(assess_with("מסמךך", d, f, None, &config)
            .details
            .contains("disabled"));
    }

    #[test]
    fn a_ranked_deeper_stem_survives_a_shallow_dictionary_collision() {
        for d in [
            Dict::of(&["מקלדת", "כשמקלדת", "שמקלדת"]),
            Dict::of(&["מקלדת", "כשמקלדת", "שמקלדת", "וכשמקלדת"]),
        ] {
            for f in [
                Freq::of(&[("מקלדת", 500)]),
                Freq::of(&[("מקלדת", 500), ("כשמקלדת", 70_000), ("שמקלדת", 60_000)]),
            ] {
                let suggestions = correction_suggestions("וכשמקלדתת", d, f, None);
                assert!(
                    suggestions.iter().any(|word| word == "וכשמקלדת"),
                    "{suggestions:?}"
                );
                assert!(!unranked_repeat_base("וכשמקלדתת", d, f));
                let mut config = Config::global();
                config.spell_enabled = true;
                config.spell_max_dist = 1;
                config.spell_max_rank = 30_000;
                assert_eq!(
                    assess_with("וכשמקלדתת", d, f, None, &config)
                        .correction
                        .as_deref(),
                    Some("וכשמקלדת")
                );
            }
        }
        let d = Dict::of(&["מקלדת", "וכשמקלדת"]);
        let f = Freq::of(&[("מקלדת", 500), ("וכשמקלדת", 70_000)]);
        assert!(
            !correction_suggestions("וכשמקלדתת", d, f, None).contains(&"וכשמקלדת".to_owned()),
            "a directly attested rare rank must retain its frequency gate"
        );
    }

    #[test]
    fn a_repeated_key_does_not_justify_an_unrelated_frequent_neighbor() {
        let assessment = assess(
            "מחחב",
            Dict::of(&["מחב", "מיחב"]),
            Freq::of(&[("מיחב", 3_000)]),
            None,
        );
        assert_eq!(assessment.correction, None, "{}", assessment.details);
        assert_eq!(assessment.best.unwrap().text, "מיחב");
        assert!(assessment.details.contains("keeping the original token"));
    }

    #[test]
    fn hebrew_assessments_explain_ambiguity_prefixes_and_protection() {
        let ambiguous = assess(
            "שלוב",
            Dict::of(&["שלום", "שלוש"]),
            Freq::of(&[("שלום", 100), ("שלוש", 100)]),
            None,
        );
        assert_eq!(ambiguous.correction, None);
        for evidence in [
            "שלום: score",
            "שלוש: score",
            "Score gap:",
            "required minimum: 20.00",
            "Keep original:",
        ] {
            assert!(
                ambiguous.details.contains(evidence),
                "{}",
                ambiguous.details
            );
        }
        let prefixed = assess(
            "במחבש",
            Dict::of(&["מחשב"]),
            Freq::of(&[("מחשב", 100)]),
            None,
        );
        assert_eq!(prefixed.correction.as_deref(), Some("במחשב"));
        assert!(prefixed.details.contains("ranked dictionary stem \"מחשב\""));
        assert!(prefixed.details.contains("cost surcharge 12"));
        assert!(assess("שלום", Dict::of(&["שלום"]), Freq::EMPTY, None)
            .details
            .contains("Protected Hebrew dictionary word"));
        assert!(assess("שָלום", Dict::of(&["שלום"]), Freq::EMPTY, None)
            .details
            .contains("attached marks"));
    }

    #[test]
    fn hebrew_learning_classifies_only_single_script_single_edit_pairs() {
        for (typed, corrected, class) in [
            ("מחבש", "מחשב", 0),
            ("שלופ", "שלום", 1),
            ("מסמךך", "מסמך", 2),
            ("ספור", "סיפור", 2),
        ] {
            assert_eq!(typo_class(typed, corrected), Some(class), "{typed}");
            assert_eq!(
                super::super::class_discount(typo_class(typed, corrected), [1; 3]),
                0.0
            );
            assert_eq!(
                super::super::class_discount(typo_class(typed, corrected), [2; 3]),
                4.0
            );
            assert_eq!(
                super::super::class_discount(typo_class(typed, corrected), [u64::MAX; 3]),
                super::super::MAX_TYPO_DISCOUNT
            );
        }
        for (typed, corrected) in [
            ("שלום", "שלום"),
            ("שלומ", "שלום"),
            ("מכחב", "מחשב"),
            ("שָלום", "שלום"),
            ("hello", "שלום"),
            ("", "שלום"),
        ] {
            assert_eq!(typo_class(typed, corrected), None, "{typed}");
        }
    }

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
