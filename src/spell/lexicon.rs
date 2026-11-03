//! Spelling trust is distinct from membership in the permissive layout dictionary.

use crate::dictionary::{Dict, Freq};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Trust {
    Technical,
    Attested,
    ReviewedMisspelling(String),
    Unattested,
    Unknown,
}

/// Reviewed nonword spellings, not regional variants or rare valid words.
/// Keep this table sorted. In particular, `calender`, `dependant`, and `form`
/// are deliberately absent: their validity cannot be decided without context.
const MISSPELLINGS: &[(&str, &str)] = &[
    ("accomodate", "accommodate"),
    ("accomodation", "accommodation"),
    ("adress", "address"),
    ("arguement", "argument"),
    ("becuase", "because"),
    ("cemetary", "cemetery"),
    ("committment", "commitment"),
    ("definately", "definitely"),
    ("enviroment", "environment"),
    ("existance", "existence"),
    ("freind", "friend"),
    ("goverment", "government"),
    ("happend", "happened"),
    ("hte", "the"),
    ("immediatly", "immediately"),
    ("indispensible", "indispensable"),
    ("irresistable", "irresistible"),
    ("langauge", "language"),
    ("millenium", "millennium"),
    ("noticable", "noticeable"),
    ("occassion", "occasion"),
    ("occassionally", "occasionally"),
    ("occurance", "occurrence"),
    ("prefered", "preferred"),
    ("priviledge", "privilege"),
    ("recieve", "receive"),
    ("recieved", "received"),
    ("recieving", "receiving"),
    ("recomend", "recommend"),
    ("refered", "referred"),
    ("relevent", "relevant"),
    ("resistence", "resistance"),
    ("rythm", "rhythm"),
    ("seige", "siege"),
    ("seperate", "separate"),
    ("seperately", "separately"),
    ("seperatly", "separately"),
    ("succesful", "successful"),
    ("succesfully", "successfully"),
    ("teh", "the"),
    ("thier", "their"),
    ("tommorow", "tomorrow"),
    ("tomorow", "tomorrow"),
    ("tounge", "tongue"),
    ("twelth", "twelfth"),
    ("withdrawl", "withdrawal"),
];

pub(super) fn reviewed_misspelling(word: &str) -> Option<&'static str> {
    MISSPELLINGS
        .binary_search_by_key(&word, |(typed, _)| *typed)
        .ok()
        .map(|index| MISSPELLINGS[index].1)
}

/// Reuse reviewed errors in regular inflections, requiring a real target word.
/// This does not make arbitrary dictionary entries into reviewed misspellings.
pub(super) fn reviewed_repair(word: &str, dict: Dict) -> Option<String> {
    if let Some(target) = reviewed_misspelling(word) {
        return Some(target.into());
    }
    for suffix in ["s", "es", "d", "ed", "ing", "ly"] {
        let Some(stem) = word.strip_suffix(suffix).filter(|stem| stem.len() >= 4) else {
            continue;
        };
        if let Some(target) = reviewed_misspelling(stem) {
            let target = format!("{target}{suffix}");
            if dict.contains(&target) {
                return Some(target);
            }
        }
        if matches!(suffix, "ed" | "ing") {
            if let Some(target) = reviewed_misspelling(&format!("{stem}e")) {
                if let Some(target) = target.strip_suffix('e') {
                    let target = format!("{target}{suffix}");
                    if dict.contains(&target) {
                        return Some(target);
                    }
                }
            }
        }
    }
    None
}

/// An unranked derivative needs an attested dictionary stem. Discount its
/// frequency evidence instead of borrowing the stem's full rank.
pub(super) fn derived_rank(word: &str, dict: Dict, freq: Freq) -> Option<u32> {
    if word.len() < 8 || !dict.contains(word) || freq.rank(word).is_some() {
        return None;
    }
    ["mis", "un", "re", "dis", "non", "over", "under"]
        .iter()
        .filter_map(|prefix| word.strip_prefix(prefix))
        .filter(|stem| stem.len() >= 4 && dict.contains(stem))
        .filter(|stem| reviewed_repair(stem, dict).is_none())
        .filter_map(|stem| freq.rank(stem).filter(|rank| *rank <= 10_000))
        .map(|rank| rank.saturating_mul(4).max(10_000))
        .min()
}

fn technical() -> Dict {
    Dict::new(include_str!(concat!(env!("OUT_DIR"), "/en_tech.blob")))
}

pub(super) fn trust(word: &str, dict: Dict, freq: Freq) -> Trust {
    if technical().contains(word) {
        Trust::Technical
    } else if let Some(target) = reviewed_repair(word, dict) {
        Trust::ReviewedMisspelling(target)
    } else if dict.contains(word) {
        if freq.rank(word).is_some() {
            Trust::Attested
        } else {
            Trust::Unattested
        }
    } else {
        Trust::Unknown
    }
}

/// Two trusted components are evidence for an intentional new compound or tool
/// name, even when the combined token has not reached a general frequency list.
pub(super) fn compound_rank(word: &str, dict: Dict, freq: Freq) -> Option<u32> {
    if !word.is_ascii() {
        return None;
    }
    let component = |part: &str| {
        if reviewed_misspelling(part).is_some() {
            None
        } else if technical().contains(part) {
            Some(freq.rank(part).unwrap_or(20_000))
        } else {
            freq.rank(part)
                .filter(|rank| *rank <= 20_000 && dict.contains(part))
        }
    };
    (3..word.len().saturating_sub(2)).find_map(|index| {
        let (first, second) = word.split_at(index);
        Some(component(first)?.max(component(second)?))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_spellings_have_valid_targets_and_exclude_real_variants() {
        assert!(MISSPELLINGS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        for &(typed, target) in MISSPELLINGS {
            assert_ne!(typed, target);
            assert!(crate::dictionary::en_dict().contains(target), "{target}");
            assert!(
                crate::dictionary::en_freq().rank(target).is_some(),
                "{target}"
            );
            assert!(!technical().contains(typed), "{typed}");
            assert_eq!(reviewed_misspelling(typed), Some(target));
        }
        for word in ["calender", "dependant", "form", "organisation"] {
            assert_eq!(reviewed_misspelling(word), None, "{word}");
        }
    }

    #[test]
    fn compounds_require_two_trusted_components() {
        let dict = Dict::of(&["cloud", "forge", "recieve", "time"]);
        let freq = Freq::of(&[("cloud", 500), ("forge", 2_000), ("recieve", 100)]);
        assert_eq!(compound_rank("cloudforge", dict, freq), Some(2_000));
        assert_eq!(compound_rank("cloudrecieve", dict, freq), None);
        assert_eq!(compound_rank("cloudtime", dict, freq), None);
        assert_eq!(compound_rank("xcloud", dict, freq), None);
    }
}
