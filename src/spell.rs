//! In-language English spelling autocorrect — the second correction pipeline.
//!
//! The layout pipeline in `dictionary.rs` compares the *same keystrokes* read
//! under two layouts. This one stays inside a single language: the keystrokes
//! read as English, but the result is not an English word, so we look for the
//! word the user meant — a near-miss of a common English word.
//!
//! Precision matters far more than recall here: a wrong layout switch is at
//! least reversible in the user's head ("I was in the wrong layout"), while a
//! wrong spelling fix silently rewrites a name or an identifier into a
//! different word. So the suggestion has to clear several gates:
//!
//! * the typed word is not itself an English word (the caller guarantees this),
//! * it is not a token the corpus sees often enough to be deliberate — a name
//!   or a handle rather than a typo (see [`correct_with`]),
//! * it is long enough to be a real typo rather than an initialism (`min_len`),
//! * the candidate is within one edit (two only when explicitly enabled),
//! * the candidate is a *common* word — present in the frequency list within
//!   `max_rank` — as well as a dictionary word,
//! * the candidate keeps the typed word's first letter (with one exception,
//!   see [`first_letter_ok`]).
//!
//! Among the candidates that survive, the one the user most likely meant wins:
//! see [`Tier`] for how "likely" is ordered.
//!
//! Everything here is pure and dictionary-driven, so it is unit testable and
//! never touches the OS.

use crate::config::Config;
use crate::dictionary::{Dict, Freq};

/// Letters a candidate may be built from. The English keymap can only produce
/// `a-z` (and digits, which are excluded from correction entirely), so
/// restricting the alphabet also guarantees every suggestion is typeable.
const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz";

/// Longest word we try to correct. Beyond this a "word" is almost certainly a
/// URL fragment, a path or an identifier, and the edit scan gets pointlessly
/// wide (it is linear in the length).
const MAX_LEN: usize = 20;

/// Distance-2 correction is only attempted from this length up: on a short word
/// two edits change so much of it that the "correction" is usually a different
/// word altogether.
const DIST2_MIN_LEN: usize = 6;

/// A distance-2 suggestion has to be this many times more common than a
/// distance-1 one would need to be — two edits is a much weaker signal, so only
/// genuinely frequent words are allowed to win that way.
const DIST2_RANK_FACTOR: u32 = 5;

/// How likely an edit is to be what the user did, independent of the resulting
/// word's frequency. Candidates are ranked by tier first and frequency second,
/// because raw frequency alone picks the wrong word surprisingly often: `helo`
/// is one edit from both `hello` and the (more common) `help`, but nobody
/// typing `helo` meant `help`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Tier {
    /// A slip of the fingers rather than a misspelling: two letters swapped, or
    /// a doubled letter typed once / a single letter typed twice. These are the
    /// most frequent real-world typos by a wide margin.
    Slip,
    /// Any other single edit — a wrong, missing or extra letter.
    Other,
}

/// Best English correction for `word`, or `None` to leave it alone.
///
/// Thresholds come from the global config (`RECAST_SPELL*`); the actual work is
/// in [`correct_with`], which takes them explicitly so tests don't depend on
/// the environment.
pub fn correct(word: &str, en_dict: Dict, en_freq: Freq) -> Option<String> {
    let cfg = Config::global();
    if !cfg.spell_enabled {
        return None;
    }
    correct_with(
        word,
        en_dict,
        en_freq,
        cfg.spell_min_len,
        cfg.spell_max_rank,
        cfg.spell_max_dist,
    )
}

/// Core of [`correct`] with the tuning knobs passed in.
pub fn correct_with(
    word: &str,
    en_dict: Dict,
    en_freq: Freq,
    min_len: usize,
    max_rank: u32,
    max_dist: u8,
) -> Option<String> {
    if max_dist == 0 || !eligible(word, min_len) {
        return None;
    }
    // A word we already know is never a typo. The caller normally checks this
    // too, but it is cheap and this must never "correct" a valid word.
    if en_dict.contains(word) {
        return None;
    }
    // Not in the dictionary, but common enough in the corpus that people
    // clearly type it on purpose: names and internet spellings ("sami", "ori",
    // "alot"). The bar is the same `max_rank` used to accept a suggestion — a
    // token we would have been willing to *suggest* is not one to overwrite.
    // (The bound matters: the tail of the corpus is itself full of misspellings
    // — "adress", "goverment" — which are exactly what we are here to fix.)
    if en_freq.rank(word).is_some_and(|rank| rank <= max_rank) {
        return None;
    }

    // Distance 1 first: it is both the likeliest kind of typo and the safest
    // correction, so a distance-1 hit always wins over any distance-2 one.
    let mut best: Option<(Tier, u32, String)> = None;
    for_each_edit1(word, |cand, tier| {
        consider(cand, word, tier, max_rank, en_dict, en_freq, &mut best);
    });
    if let Some((_, _, fixed)) = best {
        return Some(fixed);
    }

    if max_dist < 2 || word.len() < DIST2_MIN_LEN {
        return None;
    }

    // Distance 2: edits of the edits. Materialise the first ring (the second
    // ring is generated per entry) and demand a much more common word. Two
    // edits away, the individual edit kinds no longer say much about intent, so
    // everything competes on frequency alone.
    let dist2_rank = max_rank / DIST2_RANK_FACTOR;
    let mut ring: Vec<String> = Vec::new();
    for_each_edit1(word, |cand, _| ring.push(cand.to_string()));
    let mut best2: Option<(Tier, u32, String)> = None;
    for near in &ring {
        for_each_edit1(near, |cand, _| {
            consider(
                cand,
                word,
                Tier::Other,
                dist2_rank,
                en_dict,
                en_freq,
                &mut best2,
            );
        });
    }
    best2.map(|(_, _, fixed)| fixed)
}

/// Whether `word` is the kind of token we are willing to rewrite at all:
/// plain lowercase ASCII letters (no digits — `a4` is an identifier, not a
/// typo) and long enough that a one-letter edit doesn't turn one word into an
/// unrelated one.
fn eligible(word: &str, min_len: usize) -> bool {
    word.len() >= min_len
        && word.len() <= MAX_LEN
        && word.bytes().all(|b| b.is_ascii_lowercase())
}

/// Score one candidate and keep it if it beats the incumbent.
///
/// A candidate must be *both* a dictionary word and a common one: the
/// dictionary is huge (~370k entries, including archaic and technical words),
/// so membership alone would happily "correct" a name into something nobody
/// has typed since 1890. The frequency rank is what makes the suggestion the
/// word the user actually meant.
#[allow(clippy::too_many_arguments)]
fn consider(
    cand: &str,
    typed: &str,
    tier: Tier,
    max_rank: u32,
    en_dict: Dict,
    en_freq: Freq,
    best: &mut Option<(Tier, u32, String)>,
) {
    if !first_letter_ok(typed, cand) {
        return;
    }
    let Some(rank) = en_freq.rank(cand) else {
        return;
    };
    if rank > max_rank || !en_dict.contains(cand) {
        return;
    }
    if best
        .as_ref()
        .is_none_or(|(cur_tier, cur_rank, _)| (tier, rank) < (*cur_tier, *cur_rank))
    {
        *best = Some((tier, rank, cand.to_string()));
    }
}

/// Typos on the *first* letter are rare, and rewriting it is the most damaging
/// kind of wrong correction — it is what turns a name into an unrelated word.
/// So a candidate has to keep the first letter, with one exception: swapping
/// the first two letters (`hte` → `the`), which is one of the most common typos
/// there is.
fn first_letter_ok(typed: &str, cand: &str) -> bool {
    let (t, c) = (typed.as_bytes(), cand.as_bytes());
    if let (Some(a), Some(b)) = (t.first(), c.first()) {
        if a == b {
            return true;
        }
    }
    t.len() >= 2 && c.len() == t.len() && c[0] == t[1] && c[1] == t[0] && c[2..] == t[2..]
}

/// Call `f` with every string one edit away from `word` — deletions,
/// transpositions, substitutions and insertions (Damerau-Levenshtein distance
/// 1) — together with the [`Tier`] of the edit that produced it.
///
/// Candidates are handed out as borrowed `&str` from a single reused buffer, so
/// a scan allocates nothing — it runs on every finished word.
///
/// `word` is ASCII (the caller checked), so byte indexing is character
/// indexing here.
fn for_each_edit1(word: &str, mut f: impl FnMut(&str, Tier)) {
    let w = word.as_bytes();
    let n = w.len();
    let mut buf: Vec<u8> = Vec::with_capacity(n + 1);

    // Deletions: the user typed one letter too many. Dropping one half of a
    // doubled letter ("hellp" -> "help") is a finger slip; dropping a letter
    // that stands alone is a misspelling.
    for i in 0..n {
        let doubled = (i > 0 && w[i - 1] == w[i]) || (i + 1 < n && w[i + 1] == w[i]);
        buf.clear();
        buf.extend_from_slice(&w[..i]);
        buf.extend_from_slice(&w[i + 1..]);
        if let Ok(s) = std::str::from_utf8(&buf) {
            f(s, if doubled { Tier::Slip } else { Tier::Other });
        }
    }

    // Transpositions: two letters landed in the wrong order.
    for i in 0..n.saturating_sub(1) {
        buf.clear();
        buf.extend_from_slice(w);
        buf.swap(i, i + 1);
        if let Ok(s) = std::str::from_utf8(&buf) {
            f(s, Tier::Slip);
        }
    }

    // Substitutions: one letter is wrong (a neighbouring key, usually).
    for i in 0..n {
        buf.clear();
        buf.extend_from_slice(w);
        for &c in ALPHABET {
            if c == w[i] {
                continue;
            }
            buf[i] = c;
            if let Ok(s) = std::str::from_utf8(&buf) {
                f(s, Tier::Other);
            }
        }
    }

    // Insertions: a letter is missing. Restoring a doubled letter ("helo" ->
    // "hello") is the mirror of the deletion case, and just as common.
    for i in 0..=n {
        for &c in ALPHABET {
            let doubled = (i > 0 && w[i - 1] == c) || (i < n && w[i] == c);
            buf.clear();
            buf.extend_from_slice(&w[..i]);
            buf.push(c);
            buf.extend_from_slice(&w[i..]);
            if let Ok(s) = std::str::from_utf8(&buf) {
                f(s, if doubled { Tier::Slip } else { Tier::Other });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;

    fn dict(words: &[&str]) -> Dict {
        Dict::of(words)
    }

    fn freq(entries: &[(&str, u32)]) -> Freq {
        Freq::of(entries)
    }

    /// Defaults matching `Config::from_env`, so the tests exercise shipped
    /// behaviour rather than a hand-tuned configuration.
    fn fix(word: &str, d: Dict, f: Freq) -> Option<String> {
        correct_with(word, d, f, 4, 20_000, 1)
    }

    #[test]
    fn corrects_a_one_letter_typo() {
        let d = dict(&["hello", "hell"]);
        let f = freq(&[("hello", 500), ("hell", 4000)]);
        // "helo" is one insertion away from "hello" and one substitution away
        // from "hell"; the more common word wins.
        assert_eq!(fix("helo", d, f).as_deref(), Some("hello"));
    }

    #[test]
    fn a_slip_beats_a_more_common_misspelling() {
        let d = dict(&["hello", "help"]);
        // "help" is the more common word, but restoring the dropped half of the
        // "ll" is by far the likelier thing to have happened.
        let f = freq(&[("help", 140), ("hello", 203)]);
        assert_eq!(fix("helo", d, f).as_deref(), Some("hello"));
        // Same the other way round: an extra letter in a double.
        assert_eq!(fix("hellp", d, f).as_deref(), Some("help"));
    }

    #[test]
    fn a_token_the_corpus_knows_is_never_corrected() {
        // "sami" isn't a dictionary word, but it is a name people type; the
        // frequency list ranking it inside max_rank is what saves it from
        // becoming "same".
        let d = dict(&["same"]);
        let f = freq(&[("same", 240), ("sami", 18_299)]);
        assert_eq!(fix("sami", d, f), None);
        // The same word, unseen by the corpus, is treated as a typo.
        let f_unseen = freq(&[("same", 240)]);
        assert_eq!(fix("sami", d, f_unseen).as_deref(), Some("same"));
    }

    #[test]
    fn a_misspelling_in_the_corpus_tail_is_still_corrected() {
        // Corpora are transcripts of real writing, so common misspellings do
        // appear in them — far down the list. Being seen is only protection
        // when it is seen *often*.
        let d = dict(&["address"]);
        let f = freq(&[("address", 1_141), ("adress", 42_567)]);
        assert_eq!(fix("adress", d, f).as_deref(), Some("address"));
    }

    #[test]
    fn corrects_a_transposition() {
        let d = dict(&["their", "there"]);
        let f = freq(&[("their", 100)]);
        assert_eq!(fix("theri", d, f).as_deref(), Some("their"));
    }

    #[test]
    fn leaves_a_real_word_alone() {
        let d = dict(&["form", "from"]);
        let f = freq(&[("from", 10), ("form", 900)]);
        // "form" is a word, even though "from" is far more common: never
        // second-guess something the dictionary already knows.
        assert_eq!(fix("form", d, f), None);
    }

    #[test]
    fn leaves_an_unknown_word_with_no_near_match_alone() {
        let d = dict(&["hello"]);
        let f = freq(&[("hello", 500)]);
        assert_eq!(fix("zqxjk", d, f), None);
    }

    #[test]
    fn never_changes_the_first_letter() {
        // "sork" is one substitution from both "work" and "sort"; only the one
        // that keeps the first letter is allowed, even though "work" is more
        // common.
        let d = dict(&["work", "sort"]);
        let f = freq(&[("work", 100), ("sort", 900)]);
        assert_eq!(fix("sork", d, f).as_deref(), Some("sort"));
    }

    #[test]
    fn first_two_letter_transposition_is_allowed() {
        let d = dict(&["the"]);
        let f = freq(&[("the", 0)]);
        // Below the default minimum length, so it needs an explicit min_len.
        assert_eq!(
            correct_with("hte", d, f, 3, 20_000, 1).as_deref(),
            Some("the")
        );
    }

    #[test]
    fn short_words_are_left_alone_by_default() {
        // Initialisms and names ("ori", "sam") must survive untouched; the
        // default minimum length is what protects them.
        let d = dict(&["or", "orb"]);
        let f = freq(&[("or", 50), ("orb", 900)]);
        assert_eq!(fix("ori", d, f), None);
    }

    #[test]
    fn rare_words_are_not_suggested() {
        // "helo" is one edit from "helot", but nobody meant to type "helot".
        let d = dict(&["helot"]);
        let f = freq(&[("helot", 45_000)]);
        assert_eq!(fix("helo", d, f), None);
    }

    #[test]
    fn candidate_must_be_in_the_dictionary_too() {
        // Present in the frequency list (which is corpus-derived and contains
        // junk) but not a dictionary word → not a suggestion.
        let d = dict(&["hello"]);
        let f = freq(&[("helo", 300), ("hella", 800)]);
        assert_eq!(fix("hellu", d, f), None);
    }

    #[test]
    fn words_with_digits_are_never_corrected() {
        let d = dict(&["test"]);
        let f = freq(&[("test", 100)]);
        assert_eq!(fix("test1", d, f), None);
    }

    #[test]
    fn distance_two_is_off_by_default_and_stricter_when_on() {
        let d = dict(&["keyboard"]);
        let f = freq(&[("keyboard", 3_000)]);
        // Two edits (missing "y", swapped "ao") — ignored at max_dist 1.
        assert_eq!(fix("keboadr", d, f), None);
        // At max_dist 2 the rank budget is max_rank / DIST2_RANK_FACTOR, so
        // 3000 passes under 20000/5 = 4000 …
        assert_eq!(
            correct_with("keboadr", d, f, 4, 20_000, 2).as_deref(),
            Some("keyboard")
        );
        // … but the same word would not clear a tighter budget.
        assert_eq!(correct_with("keboadr", d, f, 4, 10_000, 2), None);
    }

    #[test]
    fn distance_one_wins_over_distance_two() {
        let d = dict(&["batter", "better"]);
        // "bettes" is one edit from "better" and two from "batter"; the
        // rarer-but-closer word wins.
        let f = freq(&[("batter", 100), ("better", 5_000)]);
        assert_eq!(
            correct_with("bettes", d, f, 4, 20_000, 2).as_deref(),
            Some("better")
        );
    }

    #[test]
    fn edit1_generates_each_kind_of_edit() {
        let mut seen: HashSet<String> = HashSet::new();
        for_each_edit1("abc", |c, _| {
            seen.insert(c.to_string());
        });
        assert!(seen.contains("bc"), "deletion");
        assert!(seen.contains("bac"), "transposition");
        assert!(seen.contains("abz"), "substitution");
        assert!(seen.contains("abcd"), "insertion");
        assert!(!seen.contains("abc"), "the word itself is not an edit");
    }

    #[test]
    fn edit1_tiers_slips_apart_from_misspellings() {
        let mut tier_of: Vec<(String, Tier)> = Vec::new();
        for_each_edit1("helo", |c, t| tier_of.push((c.to_string(), t)));
        let tier = |w: &str| tier_of.iter().find(|(c, _)| c == w).map(|(_, t)| *t);
        assert_eq!(tier("hello"), Some(Tier::Slip), "restores a double");
        assert_eq!(tier("hleo"), Some(Tier::Slip), "transposition");
        assert_eq!(tier("help"), Some(Tier::Other), "substitution");
        assert_eq!(tier("halo"), Some(Tier::Other), "substitution");
    }
}


/// Calibration tests against the *real* embedded word lists. The unit tests
/// above pin the rules; these pin the tuning — the thresholds only mean
/// something in terms of the actual dictionary and corpus, and a change to
/// either can silently start rewriting people's names.
#[cfg(test)]
mod real_data {
    use crate::dictionary::{en_dict, en_freq};
    use crate::spell::correct_with;

    /// The shipped defaults (`Config::from_env`).
    fn fix(word: &str) -> Option<String> {
        correct_with(word, en_dict(), en_freq(), 4, 20_000, 1)
    }

    #[test]
    fn fixes_everyday_typos() {
        for (typo, want) in [
            ("helo", "hello"),
            ("hellp", "help"),
            ("recieve", "receive"),
            ("adress", "address"),
            ("seperate", "separate"),
            ("definately", "definitely"),
            ("becuase", "because"),
            ("freind", "friend"),
            ("thier", "their"),
            ("occured", "occurred"),
            ("tomorow", "tomorrow"),
            ("goverment", "government"),
            ("keyboad", "keyboard"),
            ("wiht", "with"),
            ("taht", "that"),
            ("fomr", "form"),
            ("abput", "about"),
        ] {
            assert_eq!(fix(typo).as_deref(), Some(want), "{typo}");
        }
    }

    #[test]
    fn leaves_names_and_shorthand_alone() {
        // Names, handles and chat shorthand are the things a user would most
        // resent having rewritten.
        for word in [
            "sami", "ori", "supino", "claude", "github", "async", "struct",
            "asap", "idk", "brb", "nvm", "yeh",
        ] {
            assert_eq!(fix(word), None, "{word}");
        }
    }

    #[test]
    fn leaves_wrong_layout_gibberish_alone() {
        // The English reading of Hebrew words typed in the wrong layout
        // ("שלום", "מקלדת"). The layout pipeline handles these, and the
        // speller must not have an opinion about them.
        for word in ["akuo", "ykrbo", "nauscl"] {
            assert_eq!(fix(word), None, "{word}");
        }
    }
}
