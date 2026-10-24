//! Auto-complete: finishing a word instead of fixing one.
//!
//! The other two pipelines are *corrections* — they wait for a finished word,
//! decide it is wrong, and rewrite it. This one is the opposite: the word is
//! not finished, nothing is wrong with it, and the user has explicitly asked
//! for the rest of it. That difference is why it lives outside `spell.rs` and
//! why it is allowed to be far less conservative: a completion the user did not
//! want cost them one keypress and is undone by another, while a wrong
//! autocorrect happens without being asked for.
//!
//! Two mechanisms, both keyed off the partial word in the buffer:
//!
//! * [`completions`] — press the completion key mid-word and the word is
//!   filled in. It returns a short *ordered list*, not a single answer, because
//!   the trigger key can be tapped again: the second tap swaps in the next
//!   candidate, and the last one hands back exactly what the user typed. That
//!   is what makes a wrong first guess cost a keypress instead of a deletion,
//!   and it is why the completer is allowed to guess at all. The frequency list
//!   is sorted, so "every common word starting with `hel`" is one contiguous
//!   run of it (see `Freq::for_each_with_prefix`) and ranking them is a short
//!   walk using a small embedded prefix index, with no allocation per rejected
//!   candidate.
//! * [`expand`] — abbreviations the user wrote down themselves in
//!   `<config>/recast/abbrev.txt`, expanded when the word is finished — or
//!   offered as the first completion, since a rule the user wrote by hand
//!   beats anything inferred from a corpus.
//!
//! It also owns the session [`suppress`] list: the words an undo
//! has taken back, which nothing may correct again until restart.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use crate::config::Config;
use crate::dictionary::{Dict, Freq};

/// Exact completion can use any tracked prefix. Expensive prefix-edit search
/// stays shorter; dictionary membership still protects long identifiers/URLs.
const MAX_PREFIX_LEN: usize = crate::types::MAX_WORD_KEYS - 1;
const MAX_EDITED_PREFIX_LEN: usize = 20;
/// Two-error recovery keeps the first two letters and at least four more typed
/// letters. Shorter prefixes remain exact or single-edit requests.
const TWO_EDIT_PREFIX_MIN: usize = 6;

/// Guesses per completion page before the caller can restore the typed prefix.
/// The bounded API returns one page; repeated explicit requests can use the full
/// pool without making undo require a long walk through every possible word.
pub const MAX_CANDIDATES: usize = 4;

/// Unranked dictionary words require a substantial, exact part of the word.
/// Short prefixes still rely on corpus evidence or explicitly curated vocabulary.
const RARE_PREFIX_MIN: usize = 5;
const RARE_SUFFIX_MAX: usize = 8;

/// Phrase evidence is local to one uninterrupted typing context, never persisted.
/// Two observations are required before a learned pair influences ranking.
#[derive(Default)]
pub(crate) struct PhraseContext {
    history: crate::intent::Context,
    pairs: HashMap<(String, String), u16>,
}

impl PhraseContext {
    #[cfg(test)]
    pub(crate) fn with_model(data: &'static str) -> Self {
        Self {
            history: crate::intent::Context::with_model(data),
            ..Self::default()
        }
    }

    pub(crate) fn previous_word(&self) -> Option<&str> {
        self.history.previous_word()
    }

    pub(crate) fn clear(&mut self) {
        self.history.clear();
        self.pairs.clear();
    }

    #[cfg(test)]
    pub(crate) fn retain(&mut self, word: &str) {
        self.observe(word, true);
    }

    pub(crate) fn forget_learning(&mut self) {
        self.pairs.clear();
    }

    pub(crate) fn observe(&mut self, word: &str, learn: bool) {
        if !learn {
            self.pairs.clear();
        }
        let word = word.to_lowercase();
        if word.is_empty()
            || word.chars().count() > MAX_PREFIX_LEN
            || !(word.chars().all(|c| c.is_ascii_lowercase())
                || word.chars().all(|c| ('א'..='ת').contains(&c)))
        {
            self.history.clear();
            return;
        }
        if let Some(previous) = self.previous_word().filter(|_| learn) {
            let pair = (previous.to_owned(), word.clone());
            if self.pairs.len() < 256 || self.pairs.contains_key(&pair) {
                let count = self.pairs.entry(pair).or_default();
                *count = count.saturating_add(1);
            }
        }
        self.history.observe(word);
    }

    fn previous_word_boost(&self, word: &str) -> f64 {
        let count = if self.pairs.is_empty() {
            0
        } else {
            self.previous_word()
                .and_then(|previous| self.pairs.get(&(previous.to_owned(), word.to_owned())))
                .copied()
                .unwrap_or(0)
        };
        let personal = if count < 2 {
            1.0
        } else {
            1.0 + f64::from(count.min(4)) / 2.0
        };
        let bundled = self
            .previous_word()
            .map_or(1.0, |previous| phrase_boost(previous, word));
        personal.max(bundled)
    }

    /// Preserve the established context evidence for automatic decisions.
    pub(crate) fn boost(&self, word: &str) -> f64 {
        (self.previous_word_boost(word) * self.history.factor(word)).clamp(0.25, 16.0)
    }

    /// Explicit offers can use all eligible measured bigrams. Overlapping
    /// one-word evidence is combined once before applying longer history.
    pub(crate) fn offer_boost(&self, word: &str) -> f64 {
        (self
            .previous_word_boost(word)
            .max(self.history.phrase_prior(word))
            * self.history.factor(word))
        .clamp(0.25, 16.0)
    }

    /// Bounded context cost shared by both automatic spelling models. Context
    /// may oppose a confident guess as well as support an ambiguous one.
    pub(crate) fn automatic_discount(&self, word: &str) -> f32 {
        (20.0 * self.boost(word).ln() as f32).clamp(-8.0, 8.0)
    }
}

/// Curated and measured public phrase priors require no personal word pairs.
fn phrase_boost(previous: &str, word: &str) -> f64 {
    static PHRASES: OnceLock<HashMap<&'static str, Vec<(&'static str, f64)>>> = OnceLock::new();
    let phrases = PHRASES.get_or_init(|| {
        let mut map: HashMap<_, Vec<_>> = HashMap::new();
        for line in include_str!("phrases.tsv")
            .lines()
            .chain(include_str!("phrase_counts.tsv").lines())
            .filter(|line| !line.starts_with('#'))
        {
            let fields: Vec<_> = line.split('\t').collect();
            if let [previous, next, weight, ..] = fields.as_slice() {
                if let Ok(weight) = weight.parse::<f64>() {
                    map.entry(*previous).or_default().push((*next, weight));
                }
            }
        }
        map
    });
    phrases.get(previous).map_or(1.0, |entries| {
        entries
            .iter()
            .filter(|(next, _)| *next == word)
            .map(|(_, weight)| *weight)
            .fold(1.0, f64::max)
    })
}

#[derive(Default)]
struct Ranking<'a> {
    context: Option<&'a PhraseContext>,
    offset: usize,
    exclude: Option<&'a str>,
    all: bool,
}

/// Frequency and retained personal evidence estimate likelihood. Each prefix edit
/// receives a tenfold penalty. Letters saved give a small, bounded benefit after
/// accounting for cycle taps. Even one-letter completions retain their frequency
/// weight, so long suffixes cannot overwhelm common short words.
struct Candidate {
    word: String,
    saved: usize,
    weight: f64,
    rank: u32,
    fallback: bool,
}

impl Candidate {
    fn value(&self, taps: usize) -> f64 {
        let benefit = self.saved.saturating_sub(taps).min(4) as f64;
        (1.0 + 0.15 * benefit) * self.weight
    }
}

/// Completions to offer for the partial word `prefix`, best first.
///
/// Empty when there is nothing worth offering. The user's own abbreviation for
/// the prefix, if they defined one, always comes first.
#[cfg(test)]
pub fn completions(prefix: &str, dict: Dict, freq: Freq) -> Vec<String> {
    completions_in_context(prefix, dict, freq, None)
}

pub(crate) fn completions_in_context(
    prefix: &str,
    dict: Dict,
    freq: Freq,
    context: Option<&PhraseContext>,
) -> Vec<String> {
    completions_for_request(prefix, dict, freq, context, false)
}

/// Every eligible offer, for a caller that pages through choices on repeated requests.
/// The first page uses exactly the same ranking as the bounded completion API.
pub(crate) fn all_completions_in_context(
    prefix: &str,
    dict: Dict,
    freq: Freq,
    context: Option<&PhraseContext>,
) -> Vec<String> {
    completions_for_request(prefix, dict, freq, context, true)
}

fn completions_for_request(
    prefix: &str,
    dict: Dict,
    freq: Freq,
    context: Option<&PhraseContext>,
    all: bool,
) -> Vec<String> {
    let cfg = Config::global();
    if !cfg.complete_enabled {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(MAX_CANDIDATES + 1);
    // An abbreviation is a rule the user wrote by hand — it outranks every
    // guess, exactly as it does when the word is finished (see `plan`).
    if let Some(text) = abbreviation(prefix) {
        out.push(text);
    }
    for word in completions_from(
        prefix,
        dict,
        freq,
        cfg.complete_min_len,
        cfg.complete_max_rank,
        &saved_words(),
        Ranking {
            context,
            offset: out.len(),
            exclude: out.first().map(String::as_str),
            all,
        },
    ) {
        out.push(word);
    }
    out
}

/// Core of [`completions`] with the tuning knobs passed in, so tests don't
/// depend on the environment. Abbreviations are not consulted here.
///
/// A candidate must be a *longer* word than what was typed (completing a word
/// to itself is a no-op the caller shouldn't have to unwind), a dictionary word
/// — the frequency list is corpus-derived and full of junk tokens. Short prefixes
/// require frequency evidence or curated vocabulary; a long exact prefix can also
/// recover specific dictionary words outside the general frequency cutoff.
#[cfg(test)]
pub fn completions_with(
    prefix: &str,
    en_dict: Dict,
    en_freq: Freq,
    min_len: usize,
    max_rank: u32,
) -> Vec<String> {
    completions_from(
        prefix,
        en_dict,
        en_freq,
        min_len,
        max_rank,
        &[],
        Ranking::default(),
    )
}

/// Explicitly saved exceptions also make useful completion vocabulary.
fn saved_words() -> Vec<String> {
    let mut words = crate::personal::completion_words();
    if let Ok(list) = ignore_list().lock() {
        words.extend(list.iter().cloned());
    }
    if let Ok(list) = learned_words().lock() {
        words.extend(
            list.iter()
                .filter(|(_, count)| **count >= LEARNED_MIN)
                .map(|(w, _)| w.clone()),
        );
    }
    words.sort_unstable();
    words.dedup();
    words
}

fn completions_from(
    prefix: &str,
    dict: Dict,
    freq: Freq,
    min_len: usize,
    max_rank: u32,
    saved: &[String],
    ranking: Ranking<'_>,
) -> Vec<String> {
    let len = prefix.chars().count();
    if prefix.contains('\'') {
        return apostrophe_completions(prefix, dict, freq, min_len, max_rank, saved, ranking);
    }
    let english = prefix.chars().all(|c| c.is_ascii_lowercase());
    if len < min_len
        || len > MAX_PREFIX_LEN
        || !(english || prefix.chars().all(|c| ('א'..='ת').contains(&c)))
    {
        return Vec::new();
    }
    let mut candidates: HashMap<String, Candidate> = HashMap::new();
    // Exact and edited matches compete in the same pool. A typo needs much
    // stronger frequency evidence to beat an equally useful exact match.
    // Editing a three-letter prefix is too ambiguous to compete with an exact
    // offer. Keep short-prefix recovery only when there is no exact candidate.
    let exact = if len < 4 {
        let mut exact = false;
        freq.for_each_with_prefix(prefix, |word, rank| {
            exact |= word.chars().count() > len && rank <= max_rank && dict.contains(word);
        });
        exact |= saved
            .iter()
            .any(|word| word.starts_with(prefix) && word.chars().count() > len);
        if english {
            technical_words().for_each_with_prefix(prefix, |word| {
                exact |= word.len() > len && dict.contains(word);
            });
        }
        exact
    } else {
        false
    };
    let mut prefixes = if len < 3 || exact || len > MAX_EDITED_PREFIX_LEN {
        HashSet::new()
    } else {
        edited_prefixes(prefix, english)
    };
    prefixes.insert(prefix.to_owned());
    let mut consider = |word: &str, rank: u32, explicit: bool, fallback: bool, penalty: f64| {
        let word_len = word.chars().count();
        if word_len <= len
            || word_len > crate::types::MAX_WORD_KEYS
            || ranking.exclude == Some(word)
            || candidates.contains_key(word)
            || (!explicit && !fallback && (rank > max_rank || !dict.contains(word)))
        {
            return;
        }
        let penalty = if word.starts_with(prefix) {
            1.0
        } else {
            penalty
        };
        let weight = penalty
            * f64::from(crate::personal::personal_boost(word))
            * ranking
                .context
                .map_or(1.0, |context| context.offer_boost(word))
            / (f64::from(rank) + 1.0);
        candidates.insert(
            word.to_owned(),
            Candidate {
                word: word.to_owned(),
                saved: word_len - len,
                weight,
                rank,
                fallback,
            },
        );
    };
    for matching in &prefixes {
        freq.for_each_with_prefix(matching, |word, rank| {
            consider(word, rank, false, false, 0.1);
        });
    }
    for word in saved {
        let valid = word.chars().all(|c| {
            if english {
                c.is_ascii_graphic()
            } else {
                ('א'..='ת').contains(&c)
            }
        });
        if valid && prefixes.iter().any(|p| word.starts_with(p)) {
            consider(word, freq.rank(word).unwrap_or(1_000), true, false, 0.1);
        }
    }
    if english {
        // This reviewed list is already merged into the dictionary at build time.
        // Its terms need not appear in a general-language frequency corpus.
        for matching in &prefixes {
            technical_words().for_each_with_prefix(matching, |word| {
                if dict.contains(word) {
                    consider(word, freq.rank(word).unwrap_or(20_000), false, true, 0.1);
                }
            });
        }
    }
    if (TWO_EDIT_PREFIX_MIN..=MAX_EDITED_PREFIX_LEN).contains(&len) {
        // Search one narrow run rather than generating every pair of edits.
        // The intact opening bounds both ambiguity and work for explicit requests.
        let typed: Vec<_> = prefix.chars().collect();
        let anchor: String = typed.iter().take(2).collect();
        freq.for_each_with_prefix(&anchor, |word, rank| {
            if rank <= max_rank && within_two_prefix_edits(&typed, word) {
                consider(word, rank, false, false, 0.01);
            }
        });
        for word in saved {
            if word.starts_with(&anchor)
                && word.chars().all(|c| {
                    if english {
                        c.is_ascii_graphic()
                    } else {
                        ('א'..='ת').contains(&c)
                    }
                })
                && within_two_prefix_edits(&typed, word)
            {
                consider(word, freq.rank(word).unwrap_or(1_000), true, false, 0.01);
            }
        }
        if english {
            technical_words().for_each_with_prefix(&anchor, |word| {
                if dict.contains(word) && within_two_prefix_edits(&typed, word) {
                    consider(word, freq.rank(word).unwrap_or(20_000), false, true, 0.01);
                }
            });
        }
    }
    if len >= RARE_PREFIX_MIN {
        // Dictionary-only fallback never edits the prefix or infers a new form.
        // Requiring at least half the word and a bounded suffix keeps this useful
        // for specific requests without flooding short-prefix completion.
        dict.for_each_with_prefix(prefix, |word| {
            let word_len = word.chars().count();
            if word_len <= len * 2
                && word_len.saturating_sub(len) <= RARE_SUFFIX_MAX
                && word.chars().all(|c| {
                    if english {
                        c.is_ascii_lowercase()
                    } else {
                        ('א'..='ת').contains(&c)
                    }
                })
            {
                consider(word, freq.rank(word).unwrap_or(50_000), false, true, 1.0);
            }
        });
    }
    if !english {
        // Productive Hebrew prefixes need not have a separately listed full
        // form. Infer only exact matches from the supported prefix grammar and
        // a dictionary stem, after directly attested candidates.
        for (attached, stem_prefix) in crate::dictionary::hebrew_prefix_splits(prefix) {
            let stem_len = stem_prefix.chars().count();
            if stem_len < 2 {
                continue;
            }
            let mut inferred = |stem: &str, rank: u32| {
                if stem.chars().count() >= 3 && stem.chars().all(|c| ('א'..='ת').contains(&c)) {
                    consider(&format!("{attached}{stem}"), rank, false, true, 1.0);
                }
            };
            freq.for_each_with_prefix(stem_prefix, |stem, rank| {
                if rank <= max_rank && dict.contains(stem) {
                    inferred(stem, rank);
                }
            });
            for stem in saved {
                if stem.starts_with(stem_prefix) {
                    inferred(stem, freq.rank(stem).unwrap_or(1_000));
                }
            }
            if stem_len >= RARE_PREFIX_MIN {
                dict.for_each_with_prefix(stem_prefix, |stem| {
                    let word_len = stem.chars().count();
                    if word_len <= stem_len * 2
                        && word_len.saturating_sub(stem_len) <= RARE_SUFFIX_MAX
                    {
                        inferred(stem, freq.rank(stem).unwrap_or(50_000));
                    }
                });
            }
        }
    }
    ranked_completions(
        candidates.into_values().collect(),
        ranking.offset,
        ranking.all,
    )
}

/// Banded Damerau alignment of the typed prefix against a candidate's opening.
/// The unfinished suffix is free; insertions, deletions, substitutions, and
/// adjacent transpositions each cost one. Only the next two letters can belong
/// to the aligned prefix, so long completion suffixes do not enlarge the search.
fn within_two_prefix_edits(typed: &[char], word: &str) -> bool {
    const WIDTH: usize = MAX_EDITED_PREFIX_LEN + 3;
    let mut intended = ['\0'; WIDTH - 1];
    let mut len = 0;
    for (slot, letter) in intended.iter_mut().zip(word.chars()).take(typed.len() + 2) {
        *slot = letter;
        len += 1;
    }
    if len < typed.len().saturating_sub(2) {
        return false;
    }
    let mut previous = [3u8; WIDTH];
    let mut before_previous = previous;
    for (index, cost) in previous.iter_mut().take(len + 1).enumerate() {
        *cost = index.min(3) as u8;
    }
    for i in 1..=typed.len() {
        let mut current = [3u8; WIDTH];
        current[0] = i.min(3) as u8;
        for j in i.saturating_sub(2).max(1)..=len.min(i + 2) {
            current[j] = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + u8::from(typed[i - 1] != intended[j - 1]));
            if i >= 2
                && j >= 2
                && typed[i - 1] == intended[j - 2]
                && typed[i - 2] == intended[j - 1]
            {
                current[j] = current[j].min(before_previous[j - 2] + 1);
            }
        }
        before_previous = previous;
        previous = current;
    }
    previous[typed.len().saturating_sub(2)..=len]
        .iter()
        .any(|&cost| cost <= 2)
}

fn technical_words() -> Dict {
    Dict::new(include_str!(concat!(env!("OUT_DIR"), "/en_tech.blob")))
}

/// Complete supported English suffixes and explicitly saved apostrophe names.
/// Dictionary and frequency evidence validate inferred forms.
fn apostrophe_completions(
    prefix: &str,
    dict: Dict,
    freq: Freq,
    min_len: usize,
    max_rank: u32,
    saved: &[String],
    ranking: Ranking<'_>,
) -> Vec<String> {
    let Some((stem, partial)) = prefix.split_once('\'') else {
        return Vec::new();
    };
    if !(min_len..=MAX_PREFIX_LEN).contains(&prefix.len())
        || stem.is_empty()
        || !stem.bytes().all(|c| c.is_ascii_lowercase())
        || !partial.bytes().all(|c| c.is_ascii_lowercase())
    {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    for suffix in ["t", "s", "ll", "re", "ve", "d", "m"] {
        if !suffix.starts_with(partial) || suffix.len() == partial.len() {
            continue;
        }
        let word = format!("{stem}'{suffix}");
        if ranking.exclude == Some(word.as_str()) {
            continue;
        }
        let Some(rank) = crate::dictionary::english_form_rank(&word, dict, freq)
            .filter(|rank| *rank <= max_rank)
        else {
            continue;
        };
        let weight = f64::from(crate::personal::personal_boost(&word))
            * ranking
                .context
                .map_or(1.0, |context| context.offer_boost(&word))
            / (f64::from(rank) + 1.0);
        candidates.push(Candidate {
            saved: word.len() - prefix.len(),
            word,
            weight,
            rank,
            fallback: false,
        });
    }
    for word in saved {
        let Some((first, last)) = word.split_once('\'') else {
            continue;
        };
        if word.len() <= prefix.len()
            || word.len() > crate::types::MAX_WORD_KEYS
            || !word.starts_with(prefix)
            || first.is_empty()
            || last.is_empty()
            || !first
                .bytes()
                .chain(last.bytes())
                .all(|c| c.is_ascii_lowercase())
            || ranking.exclude == Some(word.as_str())
            || candidates.iter().any(|candidate| candidate.word == *word)
        {
            continue;
        }
        let rank = crate::dictionary::english_form_rank(word, dict, freq)
            .or_else(|| freq.rank(word))
            .unwrap_or(1_000);
        candidates.push(Candidate {
            saved: word.len() - prefix.len(),
            word: word.clone(),
            weight: f64::from(crate::personal::personal_boost(word))
                * ranking
                    .context
                    .map_or(1.0, |context| context.offer_boost(word))
                / (f64::from(rank) + 1.0),
            rank,
            fallback: false,
        });
    }
    ranked_completions(candidates, ranking.offset, ranking.all)
}

fn candidate_order(a: &Candidate, b: &Candidate, taps: usize) -> std::cmp::Ordering {
    b.fallback
        .cmp(&a.fallback)
        .then_with(|| a.value(taps).total_cmp(&b.value(taps)))
        .then_with(|| a.weight.total_cmp(&b.weight))
        .then_with(|| b.rank.cmp(&a.rank))
        .then_with(|| b.saved.cmp(&a.saved))
        .then_with(|| b.word.cmp(&a.word))
}

fn ranked_completions(mut candidates: Vec<Candidate>, offset: usize, all: bool) -> Vec<String> {
    let limit = if all {
        candidates.len()
    } else {
        MAX_CANDIDATES
    };
    let mut out = Vec::with_capacity(limit.min(candidates.len()));
    // Scores stop changing once taps exceed every saved suffix. At most
    // MAX_WORD_KEYS linear selections are needed; the remainder sorts once.
    let changing_slots = candidates
        .iter()
        .map(|candidate| candidate.saved)
        .max()
        .unwrap_or(0)
        .saturating_sub(offset)
        .max(MAX_CANDIDATES)
        .min(limit);
    for slot in 0..changing_slots {
        let taps = offset + slot + 1;
        let Some((index, _)) = candidates
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| candidate_order(a, b, taps))
        else {
            break;
        };
        out.push(candidates.swap_remove(index).word);
    }
    if all {
        candidates.sort_unstable_by(|a, b| candidate_order(b, a, usize::MAX));
        out.extend(candidates.into_iter().map(|candidate| candidate.word));
    }
    out
}

/// Prefix variants within one edit, for both supported alphabets.
fn edited_prefixes(prefix: &str, english: bool) -> HashSet<String> {
    let chars: Vec<_> = prefix.chars().collect();
    let alphabet = if english {
        "abcdefghijklmnopqrstuvwxyz"
    } else {
        "אבגדהוזחטיךכלםמןנסעףפץצקרשת"
    };
    let mut variants = HashSet::new();
    for i in 0..=chars.len() {
        for c in alphabet.chars() {
            let mut inserted = chars.clone();
            inserted.insert(i, c);
            variants.insert(inserted.iter().collect());
            if i < chars.len() && c != chars[i] {
                let mut replaced = chars.clone();
                replaced[i] = c;
                variants.insert(replaced.iter().collect());
            }
        }
        if i < chars.len() {
            let mut deleted = chars.clone();
            deleted.remove(i);
            variants.insert(deleted.iter().collect());
        }
        if i + 1 < chars.len() && chars[i] != chars[i + 1] {
            let mut swapped = chars.clone();
            swapped.swap(i, i + 1);
            variants.insert(swapped.iter().collect());
        }
    }
    variants.remove(prefix);
    variants
}

/// The expansion configured for `word`, if the user defined one.
///
/// Matching is case-insensitive on the key; the expansion is reproduced exactly
/// as written, and the caller re-applies the capitalization the user typed.
pub fn expand(word: &str) -> Option<String> {
    if !Config::global().complete_enabled || word.is_empty() {
        return None;
    }
    abbreviation(word)
}

/// The expansion the user defined for `key`, if any.
fn abbreviation(key: &str) -> Option<String> {
    abbreviations().lock().ok()?.get(key).cloned()
}

/// Whether the user has declared `word` off limits in
/// `<config>/recast/ignore.txt` — one token per line, `#` for comments.
///
/// The wider the speller's edit budget gets, the more jargon it can reach: an
/// eight-letter token two slips from a top-few-thousand word is exactly what it
/// is built to fix, and `hostname` is exactly that shape. Rather than tune the
/// thresholds until nobody's vocabulary is served, this is the escape hatch for
/// the handful of words each user actually types.
pub fn ignored(word: &str) -> bool {
    ignore_list().lock().is_ok_and(|list| list.contains(word))
}

/// Take `word` off both lists, `ignore.txt` included.
///
/// The counterpart of [`suppress`], and the reason the gesture is worth having
/// in this direction too: a list you can only add to is one you eventually stop
/// trusting. Editing the file is a real edit to something the user owns, so it
/// is done conservatively — only lines that *are* this word are dropped,
/// comments and everything else are copied through untouched, and the write
/// goes via a temporary file so an interrupted save can't leave a half-written
/// list behind.
pub fn unlist(word: &str) {
    let word = word.to_lowercase();
    if let Ok(mut set) = suppressed_words().lock() {
        set.remove(&word);
    }
    // Including what earlier undos taught: the gesture is asking for this word
    // to be corrected after all, and a count from a previous session would
    // quietly decline.
    unlearn(&word);
    let was_in_file = ignore_list()
        .lock()
        .map(|mut list| list.remove(&word))
        .unwrap_or(false);
    if was_in_file {
        remove_from_ignore_file(&word);
    }
}

/// Drop every line of `ignore.txt` that is `word`, keeping the rest verbatim.
fn remove_from_ignore_file(word: &str) {
    let Some(path) = user_path("ignore.txt") else {
        return;
    };
    remove_from_ignore_path(&path, word);
}

fn remove_from_ignore_path(path: &std::path::Path, word: &str) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    // Preserve managed symlinks and never follow an existing temporary file.
    let _ = crate::settings::write_atomic(path, &without_word(&text, word));
}

/// `text` without the lines that list `word`. Everything else — comments,
/// blanks, spacing, the other entries — is copied through exactly as written:
/// this is the user's file, and the gesture has a mandate for one line of it.
fn without_word(text: &str, word: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim();
        // A comment is never a listing, whatever it says.
        if !trimmed.starts_with('#') && trimmed.to_lowercase() == word {
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    kept
}

// ─────────────────────────────────────────────────────────────────────────────
// What the undo gesture teaches, kept past the end of the session
// ─────────────────────────────────────────────────────────────────────────────

/// How many undos of the same word retire it for good.
///
/// Two, not one. A single undo already stops the word being corrected for the
/// rest of the session ([`suppress`]), which is the right answer for a word the
/// user took back by reflex — and a reflex is not a decision. Doing it twice, on
/// two separate occasions, is: it means the correction is not a one-off
/// annoyance but something that will keep happening, and that is exactly what
/// `ignore.txt` is for. This is the same conclusion reached without asking the
/// user to go and find a file.
const LEARNED_MIN: u32 = 2;

/// The undo counts, read from `learned.txt` on first use.
///
/// The gap this fills: the session list is forgotten at restart and
/// `ignore.txt` has to be written by hand, so a name the speller dislikes was
/// undone once a session, for as long as the user kept using the daemon. Nothing
/// in between remembered anything. This does — and it remembers the one signal
/// there is real evidence behind, since an undo is the user saying, about a
/// specific word, that we were wrong.
///
/// Deliberately *not* fed by "a word that was typed and not corrected": the
/// speller only ever leaves those alone anyway, so counting them would gather
/// evidence about every word except the ones this exists to protect.
fn learned_words() -> &'static Mutex<HashMap<String, u32>> {
    static WORDS: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    WORDS.get_or_init(|| Mutex::new(parse_learned(&read_user_file(LEARNED_FILE))))
}

/// `<config dir>/learned.txt` — beside `ignore.txt`, and the same idea kept by
/// hand rather than by gesture.
const LEARNED_FILE: &str = "learned.txt";

/// One `word<TAB>count` per line, `#` starting a comment. A line without a
/// count, or with one that does not parse, is read as a single undo — the file
/// is meant to be editable, and "just put the word in" should work.
fn parse_learned(text: &str) -> HashMap<String, u32> {
    let mut counts = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (word, count) = match line.split_once('\t') {
            Some((word, count)) => (word.trim(), count.trim().parse().unwrap_or(1)),
            None => (line, 1),
        };
        if !word.is_empty() {
            counts.insert(word.to_lowercase(), count);
        }
    }
    counts
}

/// Serialise the counts, newest-largest first so the file reads as a list of
/// what the user most disagrees with.
fn learned_text(counts: &HashMap<String, u32>) -> String {
    let mut entries: Vec<_> = counts.iter().collect();
    entries.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let mut out = String::from(
        "# Words you have taken back a correction of, and how many times.\n\
         # Written by ReCast; safe to edit or delete.\n",
    );
    for (word, count) in entries {
        out.push_str(word);
        out.push('\t');
        out.push_str(&count.to_string());
        out.push('\n');
    }
    out
}

/// Note that the user undid a correction of `word`.
///
/// Written through to disk immediately, as `ignore.txt` is: an undo is a
/// deliberate gesture that happens a handful of times an hour, and the file is a
/// few hundred bytes. Nothing here is on the path a keystroke takes.
#[allow(dead_code)]
pub fn learn(word: &str) {
    let word = word.trim().to_lowercase();
    if word.is_empty() {
        return;
    }
    let Ok(mut counts) = learned_words().lock() else {
        return;
    };
    // The engine releases its injection gate before saving. A newer unlist
    // must win over an undo worker that has not reached persistence yet.
    // Check under the counts lock so a concurrent unlearn removes our update.
    if !suppressed(&word) {
        return;
    }
    let count = counts.entry(word).or_insert(0);
    *count = count.saturating_add(1);
    write_learned(&counts);
}

/// Forget what the undos taught about `word` — the un-ignore gesture's half of
/// the toggle. A user asking for a word to be corrected after all must not have
/// it declined by a count from last month.
fn unlearn(word: &str) {
    let Ok(mut counts) = learned_words().lock() else {
        return;
    };
    if counts.remove(word).is_some() {
        write_learned(&counts);
    }
}

/// Whether `word` has been undone often enough to be left alone for good.
pub fn learned(word: &str) -> bool {
    learned_words().lock().is_ok_and(|counts| {
        counts
            .get(&word.to_lowercase())
            .is_some_and(|n| *n >= LEARNED_MIN)
    })
}

/// Replace `learned.txt` with `counts`, via a temporary file and a rename so an
/// interrupted write cannot leave a truncated list behind.
///
/// Unlike `ignore.txt` this file is entirely ours, so it is rewritten wholesale
/// rather than appended to a line at a time — there is no user formatting to
/// preserve.
fn write_learned(counts: &HashMap<String, u32>) {
    let Some(path) = user_path(LEARNED_FILE) else {
        return;
    };
    let _ = crate::settings::write_atomic(&path, &learned_text(counts));
}

/// Words the user has taken back with the undo gesture this session.
///
/// Undo has to do more than put the letters back. A correction is a *function*
/// of what was typed: retype the same word and the same pipeline reaches the
/// same conclusion, so an undo that only rewrites the screen leaves the user on
/// a treadmill — which is what made the previous escape hatch (edit
/// `ignore.txt`, restart the daemon) the only real one. Undoing a word
/// therefore also retires it: nothing corrects it again until restart.
///
/// Deliberately not persisted: this is the list you land on by reflex, and
/// `ignore.txt` is the one you land on by deciding. [`unlist`] clears entries
/// from both.
fn suppressed_words() -> &'static Mutex<SuppressList> {
    static WORDS: OnceLock<Mutex<SuppressList>> = OnceLock::new();
    WORDS.get_or_init(|| Mutex::new(SuppressList::default()))
}

/// How many undone words are remembered at once.
///
/// The list grew without limit before: every undo added an entry and only the
/// explicit un-ignore gesture ever removed one, so a long-running daemon —
/// which is how this program is meant to run, for weeks — accumulated a word
/// per undo forever. Nothing here is worth unbounded memory.
///
/// 256 is far past what the list is for. It exists so that a word you just
/// took back is not corrected again on the next line; a word you undid two
/// hundred words ago and have not typed since is one `ignore.txt` should be
/// holding instead, which is the gesture's other half.
const MAX_SUPPRESSED: usize = 256;

/// Undone words, newest kept: a set for the lookup, and the order they arrived
/// in so the oldest can be dropped once the list is full.
#[derive(Default)]
struct SuppressList {
    set: HashSet<String>,
    order: std::collections::VecDeque<String>,
}

impl SuppressList {
    fn insert(&mut self, word: String) {
        if !self.set.insert(word.clone()) {
            return; // already listed; leave its position alone
        }
        self.order.push_back(word);
        while self.order.len() > MAX_SUPPRESSED {
            if let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
            }
        }
    }

    fn remove(&mut self, word: &str) {
        if self.set.remove(word) {
            self.order.retain(|w| w != word);
        }
    }

    fn contains(&self, word: &str) -> bool {
        self.set.contains(word)
    }
}

/// Stop correcting `word` for the rest of the session (see
/// [`suppressed_words`]). Called by the undo gesture with the reading the user
/// actually typed.
pub fn suppress(word: &str) {
    if word.is_empty() {
        return;
    }
    if let Ok(mut set) = suppressed_words().lock() {
        set.insert(word.to_lowercase());
    }
}

/// Whether `word` has been undone this session.
pub fn suppressed(word: &str) -> bool {
    suppressed_words()
        .lock()
        .is_ok_and(|set| set.contains(&word.to_lowercase()))
}

/// Path of a user list: `<config dir>/recast/<name>`.
pub fn user_path(name: &str) -> Option<std::path::PathBuf> {
    Some(config_dir()?.join(name))
}

/// Where ReCast keeps the user's files — `~/.config/recast` and its
/// per-OS equivalents.
#[cfg(not(test))]
pub fn config_dir() -> Option<std::path::PathBuf> {
    Some(dirs::config_dir()?.join("recast"))
}

// Tests exercise undo's persistence too; never read or modify the real lists.
#[cfg(test)]
pub fn config_dir() -> Option<std::path::PathBuf> {
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    Some(
        DIR.get_or_init(|| {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            std::env::temp_dir().join(format!("recast-tests-{}-{unique}", std::process::id()))
        })
        .clone(),
    )
}

/// The ignore list, read from disk on first use. Behind a `Mutex` rather than
/// straight in a `OnceLock` because it is not immutable for the life of the
/// process: [`unlist`] takes entries out of it, [`ignore_word`] puts them in,
/// and [`reload_user_files`] replaces it wholesale when the file is edited.
fn ignore_list() -> &'static Mutex<HashSet<String>> {
    static LIST: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    LIST.get_or_init(|| Mutex::new(parse_ignore_list(&read_user_file("ignore.txt"))))
}

/// Parse the ignore file: one word per line, `#` starting a comment, folded to
/// lowercase to match the (lowercase) reading of the key buffer.
fn parse_ignore_list(text: &str) -> std::collections::HashSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_lowercase)
        .collect()
}

/// The abbreviation table, read on first use and again whenever the file
/// changes. A missing or unreadable file simply means "no abbreviations" — this
/// is an optional convenience, not something worth failing startup or nagging
/// about.
fn abbreviations() -> &'static Mutex<HashMap<String, String>> {
    static TABLE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(parse_abbreviations(&read_user_file("abbrev.txt"))))
}

/// The text of one of the user's list files, or empty if it isn't there.
fn read_user_file(name: &str) -> String {
    user_path(name)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
}

/// Re-read `abbrev.txt` and `ignore.txt` from disk.
///
/// The session's undo list is deliberately left alone: those are words the user
/// took back with a gesture minutes ago, and a reload is a statement about the
/// files, not about that.
pub fn reload_user_files() {
    if let Ok(mut table) = abbreviations().lock() {
        *table = parse_abbreviations(&read_user_file("abbrev.txt"));
    }
    if let Ok(mut list) = ignore_list().lock() {
        *list = parse_ignore_list(&read_user_file("ignore.txt"));
    }
}

/// Add `word` to the ignore list and to `ignore.txt`, so nothing corrects it
/// again — the counterpart of [`unlist`], for the user who has just seen a
/// correction they never want repeated.
///
/// Appends rather than rewrites: the file belongs to the user, and adding a
/// line is the smallest possible edit to it.
///
/// Used by the tray and Linux window's recent-corrections lists.
pub fn ignore_word(word: &str) {
    let word = word.trim().to_lowercase();
    if word.is_empty() {
        return;
    }
    let already = ignore_list()
        .lock()
        .map(|mut list| !list.insert(word.clone()))
        .unwrap_or(true);
    if already {
        return;
    }
    let Some(path) = user_path("ignore.txt") else {
        return;
    };
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = file.write_all(appended_line(&existing, &word).as_bytes());
    }
}

/// What to append to `existing` to list `word`.
///
/// A file whose last line has no newline of its own would otherwise gain a
/// line reading `previouswordnewword`, listing neither — and the user's last
/// entry would stop working, which is a strange thing to have happen from
/// clicking a menu item about a different word.
///
fn appended_line(existing: &str, word: &str) -> String {
    let lead = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    format!("{lead}{word}\n")
}

/// How many abbreviations and ignored words are loaded — what `--status`
/// reports, so a user who has just edited a file can see it took.
pub fn list_counts() -> (usize, usize, usize) {
    (
        abbreviations().lock().map(|t| t.len()).unwrap_or(0),
        ignore_list().lock().map(|l| l.len()).unwrap_or(0),
        // Only the words that have actually crossed the threshold: a single
        // undo is in the file but is not yet doing anything, and reporting it
        // as a retired word would be a lie about why a correction still fires.
        learned_words()
            .lock()
            .map(|c| c.values().filter(|n| **n >= LEARNED_MIN).count())
            .unwrap_or(0),
    )
}

/// How often the user's list files are checked for edits, where they have to be
/// checked at all. Slow enough to be cheap (two `stat`s), fast enough that
/// adding an abbreviation and typing it feels like the same action.
const WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// The files this watches, and the only ones the user is meant to edit.
const WATCHED: [&str; 2] = ["abbrev.txt", "ignore.txt"];

/// Watch `abbrev.txt` and `ignore.txt` for edits and reload them in place.
///
/// Reading these once at startup made the files a restart away from taking
/// effect, which for `abbrev.txt` is most of the cost of using it at all: an
/// abbreviation is written *because* you are about to type it.
///
/// On Linux this blocks on `inotify` and costs nothing at all until something
/// changes. Everywhere else it polls — see [`poll_watch`] for why that is worth
/// the difference rather than a filesystem-watch dependency on three platforms.
pub fn spawn_watcher() {
    // Named so that a stray thread in `top -H` or a debugger identifies itself
    // instead of showing up as another anonymous copy of the process name.
    let spawned = std::thread::Builder::new()
        .name("recast-watch".into())
        .spawn(watch_forever);
    // A thread that cannot be created is not worth failing startup over: the
    // lists still load once at startup and the tray's "Reload lists" still
    // works. Only the automatic pickup is lost.
    if spawned.is_err() {
        eprintln!("Could not start the list watcher — edits to abbrev.txt and ignore.txt will need a reload.");
    }
}

/// Block until one of the watched files changes, reload, repeat.
#[cfg(target_os = "linux")]
fn watch_forever() {
    use nix::sys::inotify::{InitFlags, Inotify};

    // The *directory*, not the two files. Editors overwhelmingly save by
    // writing a temporary file and renaming it over the target, which replaces
    // the inode — a watch on the file itself would survive exactly one save and
    // then be watching something that no longer has a name.
    let Some(dir) = config_dir() else {
        return poll_watch();
    };
    // It may not exist yet: the user has never written either file. Creating it
    // is reasonable here — it is our own directory, and the alternative is
    // watching nothing until a restart that happens to come after they save.
    if std::fs::create_dir_all(&dir).is_err() {
        return poll_watch();
    }

    let Ok(inotify) = Inotify::init(InitFlags::empty()) else {
        return poll_watch();
    };
    if inotify.add_watch(&dir, watch_flags()).is_err() {
        drop(inotify);
        return poll_watch();
    }

    loop {
        // Blocks. No timer, no wakeups, nothing scheduled — the whole point of
        // this over the poll it replaces.
        match watch_once(&inotify) {
            Ok(WatchEvent::Reload) => reload_user_files(),
            Ok(WatchEvent::Unrelated) => {}
            // Directory lifecycle events have no filename and are returned as
            // successful reads. A moved watch still follows the old inode.
            Ok(WatchEvent::Lost) | Err(_) => {
                drop(inotify);
                return poll_watch();
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn watch_flags() -> nix::sys::inotify::AddWatchFlags {
    use nix::sys::inotify::AddWatchFlags as Flags;
    // Track both ordinary file saves and loss of the directory's pathname.
    Flags::IN_CLOSE_WRITE
        | Flags::IN_MOVED_TO
        | Flags::IN_CREATE
        | Flags::IN_DELETE
        | Flags::IN_DELETE_SELF
        | Flags::IN_MOVE_SELF
}

#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
enum WatchEvent {
    Unrelated,
    Reload,
    Lost,
}

#[cfg(target_os = "linux")]
fn watch_once(inotify: &nix::sys::inotify::Inotify) -> nix::Result<WatchEvent> {
    use nix::sys::inotify::AddWatchFlags as Flags;
    let events = inotify.read_events()?;
    let lost = Flags::IN_IGNORED | Flags::IN_DELETE_SELF | Flags::IN_MOVE_SELF | Flags::IN_UNMOUNT;
    if events.iter().any(|event| event.mask.intersects(lost)) {
        return Ok(WatchEvent::Lost);
    }
    let changed = events.iter().any(|event| {
        event.mask.contains(Flags::IN_Q_OVERFLOW)
            || event
                .name
                .as_ref()
                .and_then(|name| name.to_str())
                .is_some_and(|name| WATCHED.contains(&name))
    });
    Ok(if changed {
        WatchEvent::Reload
    } else {
        WatchEvent::Unrelated
    })
}

/// Modification-time polling, for the platforms without a watch this cheap.
///
/// macOS and Windows both have an equivalent — FSEvents and
/// `ReadDirectoryChangesW` — but each is a chunk of FFI, and the thing being
/// saved is two `stat`s every couple of seconds on files that are almost always
/// absent. That is not the same trade as on Linux, where the daemon is expected
/// to run for weeks and this was the only thing keeping it from being fully
/// idle.
#[cfg(not(target_os = "linux"))]
fn watch_forever() {
    poll_watch()
}

fn poll_watch() {
    let mut last = poll_once(None);
    loop {
        std::thread::sleep(WATCH_INTERVAL);
        last = poll_once(Some(last));
    }
}

type FileStamps = [Option<std::time::SystemTime>; WATCHED.len()];

fn poll_once(last: Option<FileStamps>) -> FileStamps {
    let now = WATCHED.map(|name| {
        user_path(name)
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|metadata| metadata.modified().ok())
    });
    // Capture the baseline before reloading, so an edit during that reload
    // remains visible on the next poll. Always reload when taking over a lost
    // watch: its replacement directory may already contain the user's edits.
    if last != Some(now) {
        reload_user_files();
    }
    now
}

/// Parse the abbreviation file: one `abbreviation = expansion` per line, `=` or
/// a tab as the separator, `#` starting a comment line. Keys are lowercased
/// (the buffer only ever holds lowercase readings) and blank or malformed lines
/// are skipped rather than rejected — a typo in the file should cost the user
/// that one line, not the whole table.
fn parse_abbreviations(text: &str) -> HashMap<String, String> {
    let mut table = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=').or_else(|| line.split_once('\t')) else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if key.is_empty() || value.is_empty() {
            continue;
        }
        table.insert(key.to_lowercase(), value.to_string());
    }
    table
}

#[cfg(test)]
mod learned_list {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn learned_saves_preserve_unrelated_symlink_targets() {
        const CHILD: &str = "RECAST_LEARNED_SYMLINK_TEST";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "complete::learned_list::learned_saves_preserve_unrelated_symlink_targets",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let dir = config_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LEARNED_FILE);
        let unrelated = dir.join("unrelated.txt");
        std::fs::write(&unrelated, "keep this file\n").unwrap();
        let old_temp = path.with_extension("txt.tmp");
        std::os::unix::fs::symlink(&unrelated, &old_temp).unwrap();
        let counts = HashMap::from([("example".into(), 2)]);
        write_learned(&counts);
        assert_eq!(
            std::fs::read_to_string(&unrelated).unwrap(),
            "keep this file\n"
        );
        assert!(std::fs::symlink_metadata(&old_temp)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            parse_learned(&std::fs::read_to_string(&path).unwrap()),
            counts
        );

        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&unrelated, &path).unwrap();
        write_learned(&counts);
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(&unrelated).unwrap(),
            "keep this file\n"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn learning_preserves_a_maximum_count_from_the_user_file() {
        const CHILD: &str = "RECAST_LEARNED_MAX_TEST";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "complete::learned_list::learning_preserves_a_maximum_count_from_the_user_file",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let dir = config_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LEARNED_FILE);
        std::fs::write(&path, format!("example\t{}\n", u32::MAX)).unwrap();
        assert!(learned("example"));
        suppress("example");
        learn("example");
        assert!(learned("example"));
        assert_eq!(
            parse_learned(&std::fs::read_to_string(&path).unwrap())["example"],
            u32::MAX
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_count_survives_being_written_and_read_back() {
        let mut counts = HashMap::new();
        counts.insert("supino".to_string(), 3);
        counts.insert("recast".to_string(), 1);
        assert_eq!(parse_learned(&learned_text(&counts)), counts);
    }

    #[test]
    fn the_file_is_editable_by_hand() {
        // Comments and blanks are skipped, and a bare word with no count is
        // read as one undo — "just put the word in" has to work, because that
        // is what a user editing this file will do.
        let counts = parse_learned(
            "# mine\n\
             \n\
             supino\t4\n\
             ori\n\
             Github\t2\n\
             \tnonsense\n",
        );
        assert_eq!(counts.get("supino"), Some(&4));
        assert_eq!(counts.get("ori"), Some(&1), "a bare word counts once");
        assert_eq!(counts.get("github"), Some(&2), "folded like every reading");
        assert_eq!(counts.get("#"), None);
        assert_eq!(counts.len(), 4, "the empty word is not an entry");
    }

    #[test]
    fn one_undo_is_a_reflex_and_two_are_a_decision() {
        // The threshold is the whole design: undoing once already retires the
        // word for the session, so this only has to catch the word that keeps
        // coming back.
        let mut counts = HashMap::new();
        counts.insert("sami".to_string(), 1);
        assert!(counts["sami"] < LEARNED_MIN, "one undo does not stick");
        counts.insert("sami".to_string(), 2);
        assert!(counts["sami"] >= LEARNED_MIN);
    }

    #[test]
    fn the_file_leads_with_what_is_most_disagreed_with() {
        let mut counts = HashMap::new();
        counts.insert("once".to_string(), 1);
        counts.insert("often".to_string(), 9);
        counts.insert("twice".to_string(), 2);
        let text = learned_text(&counts);
        let listed: Vec<&str> = text
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.split('\t').next())
            .collect();
        assert_eq!(listed, ["often", "twice", "once"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(words: &[&str]) -> Dict {
        Dict::of(words)
    }

    fn freq(entries: &[(&str, u32)]) -> Freq {
        Freq::of(entries)
    }

    /// Every candidate, in offer order, under the shipped defaults
    /// (`Config::from_env`).
    fn offers(prefix: &str, d: Dict, f: Freq) -> Vec<String> {
        completions_with(
            prefix,
            d,
            f,
            crate::config::DEFAULT_COMPLETE_MIN_LEN,
            crate::config::DEFAULT_COMPLETE_MAX_RANK,
        )
    }

    /// What the first tap of the completion key puts on screen.
    fn finish(prefix: &str, d: Dict, f: Freq) -> Option<String> {
        offers(prefix, d, f).into_iter().next()
    }

    #[test]
    fn common_short_completions_keep_their_frequency_weight() {
        let d = dict(&["hello", "help", "helmet"]);
        let f = freq(&[("hello", 500), ("help", 140), ("helmet", 9_000)]);
        assert_eq!(finish("hel", d, f).as_deref(), Some("help"));
        assert_eq!(offers("hel", d, f), ["help", "hello", "helmet"]);
    }

    #[test]
    fn apostrophe_completion_uses_canonical_forms_and_frequency_evidence() {
        let d = dict(&["do", "dont", "you", "youre", "keyboard", "i", "im", "ill"]);
        let f = freq(&[
            ("dont", 10),
            ("youre", 20),
            ("keyboard", 30),
            ("im", 40),
            ("ill", 50),
        ]);
        for (prefix, expected) in [
            ("don'", "don't"),
            ("you'r", "you're"),
            ("keyboard'", "keyboard's"),
        ] {
            assert_eq!(finish(prefix, d, f).as_deref(), Some(expected), "{prefix}");
        }
        assert_eq!(completions_with("i'", d, f, 2, 30_000), ["i'm", "i'll"]);
        for prefix in [
            "don't",
            "don''",
            "don'zz",
            "don_1'",
            "http://don'",
            "keyb'oard",
            "שלום'",
        ] {
            assert!(offers(prefix, d, f).is_empty(), "{prefix}");
        }
        assert!(offers("don'", d, Freq::EMPTY).is_empty());
        assert!(offers("unknown'", d, f).is_empty());
        assert!(completions_with("don'", d, f, 3, 5).is_empty());
    }

    #[test]
    fn apostrophe_completion_preserves_saved_names_and_deduplicates_forms() {
        let saved = [
            "o'connor",
            "o'connor",
            "o'conel",
            "o'cooper",
            "o'corin",
            "o'cove",
            "o''connor",
            "o'con_nor",
            "o'co1",
            "o'co/nor",
            "o'coשלום",
        ]
        .map(str::to_owned);
        let names = completions_from(
            "o'co",
            Dict::of(&[]),
            Freq::EMPTY,
            3,
            30_000,
            &saved,
            Ranking::default(),
        );
        assert_eq!(names.first().map(String::as_str), Some("o'connor"));
        assert_eq!(names.len(), MAX_CANDIDATES);
        assert!(names.iter().all(|word| !word.contains("''")
            && word.bytes().all(|c| c.is_ascii_lowercase() || c == b'\'')));
        assert_eq!(names.iter().filter(|word| *word == "o'connor").count(), 1);
        let d = dict(&["dont"]);
        let f = freq(&[("dont", 10)]);
        assert_eq!(
            completions_from(
                "don'",
                d,
                f,
                3,
                30_000,
                &["don't".into()],
                Ranking::default()
            ),
            ["don't"]
        );
    }

    #[test]
    fn saved_apostrophe_name_completion_preserves_typed_capitalization() {
        let inserted = ignore_list().lock().unwrap().insert("o'connor".into());
        let keys: Vec<_> = "O'co"
            .chars()
            .map(|c| (c.to_ascii_lowercase(), c.is_ascii_uppercase()))
            .collect();
        let offers = crate::dictionary::complete_candidates(
            &keys,
            |key| Some(key.0),
            |key| key.1,
            crate::dictionary::en_dict(),
            Some(crate::types::Language::English),
            None,
        );
        if inserted {
            ignore_list().lock().unwrap().remove("o'connor");
        }
        assert!(offers.iter().any(|word| word == "O'connor"), "{offers:?}");
    }

    #[test]
    fn long_suffixes_have_a_bounded_benefit() {
        let d = dict(&["compute", "computer", "computationally"]);
        let f = freq(&[("compute", 100), ("computer", 80), ("computationally", 250)]);
        assert_eq!(
            offers("comp", d, f),
            ["computer", "compute", "computationally"]
        );
        let d = dict(&["abcdefgh", "abcdefghijklmnop"]);
        let f = freq(&[("abcdefgh", 100), ("abcdefghijklmnop", 100)]);
        assert_eq!(finish("abc", d, f).as_deref(), Some("abcdefgh"));
    }

    #[test]
    fn completion_handles_hebrew_saved_words_and_single_prefix_typos() {
        let d = dict(&["keyboard", "keynote"]);
        let f = freq(&[("keyboard", 100), ("keynote", 500)]);
        for prefix in ["keyb", "keyba", "keybo", "keyob", "keyxb", "kexb"] {
            assert_eq!(
                finish(prefix, d, f).as_deref(),
                Some("keyboard"),
                "{prefix}"
            );
        }
        assert_eq!(finish("key", d, f).as_deref(), Some("keyboard"));
        assert!(
            offers("kxxb", d, f).is_empty(),
            "two edits must not be offered"
        );
        let hebrew = dict(&["שלום", "שלומות"]);
        let ranks = freq(&[("שלום", 100), ("שלומות", 500)]);
        assert!(completions_with("של", hebrew, ranks, 3, 30_000).is_empty());
        assert_eq!(finish("שלו", hebrew, ranks).as_deref(), Some("שלום"));
        assert_eq!(finish("שול", hebrew, ranks).as_deref(), Some("שלום"));
        let saved = [
            "supino",
            "api_v2",
            "node.js",
            "supino",
            "supino name",
            "supinoשלום",
        ]
        .map(str::to_owned);
        assert_eq!(
            completions_from(
                "sup",
                Dict::of(&[]),
                Freq::EMPTY,
                3,
                30_000,
                &saved,
                Ranking::default()
            ),
            ["supino"]
        );
        assert_eq!(
            completions_from(
                "api",
                Dict::of(&[]),
                Freq::EMPTY,
                3,
                30_000,
                &saved,
                Ranking::default()
            ),
            ["api_v2"]
        );
        assert_eq!(
            completions_from(
                "nod",
                Dict::of(&[]),
                Freq::EMPTY,
                3,
                30_000,
                &saved,
                Ranking::default()
            ),
            ["node.js"]
        );
        // Strong exact offers still lead penalized approximate matches.
        assert_eq!(
            offers("keyb", d, f).first().map(String::as_str),
            Some("keyboard")
        );
        let d = dict(&["baked", "based"]);
        let f = freq(&[("baked", 100), ("based", 100)]);
        for _ in 0..10 {
            assert_eq!(offers("baxed", d, f), Vec::<String>::new());
            assert_eq!(offers("bax", d, f), ["baked", "based"]);
        }
    }

    #[test]
    fn bundled_phrases_work_without_learning_in_both_languages() {
        let mut context = PhraseContext::default();
        context.observe("good", false);
        assert_eq!(context.boost("morning"), 4.0);
        assert_eq!(context.boost("mourning"), 1.0);
        context.observe("תודה", false);
        assert_eq!(context.boost("רבה"), 4.0);
        assert!(context.pairs.is_empty());
        context.clear();
        assert_eq!(context.boost("רבה"), 1.0);
        let mut seen = HashSet::new();
        for line in include_str!("phrases.tsv")
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            let fields: Vec<_> = line.split('\t').collect();
            assert_eq!(fields.len(), 3);
            assert!(seen.insert((fields[0], fields[1])));
            assert!((1.0..=4.0).contains(&fields[2].parse::<f64>().unwrap()));
            assert!(fields[..2]
                .iter()
                .all(|word| !word.is_empty() && word.chars().all(char::is_alphabetic)));
        }
    }

    #[test]
    fn longer_context_changes_completion_and_reset_discards_it() {
        // Synthetic measured-count fixture, independent of corpus frequencies.
        let model = "at\t100\t1000\nat the\t100\t100\nat the bank\t12\t100\nband\t20\t1000\nbank\t20\t1000\njoined\t100\t1000\njoined the\t100\t100\njoined the band\t12\t100\nthe\t100\t1000\nthe band\t12\t100\nthe bank\t12\t100\n";
        let mut context = PhraseContext::with_model(model);
        let d = dict(&["bank", "band"]);
        // Equal unigram ranks isolate the evidence added by longer history.
        let f = freq(&[("bank", 100), ("band", 100)]);
        let offers = |context: &PhraseContext| completions_in_context("ban", d, f, Some(context));
        for word in ["at", "the"] {
            context.observe(word, false);
        }
        assert_eq!(offers(&context)[0], "bank");
        context.clear();
        for word in ["joined", "the"] {
            context.observe(word, false);
        }
        assert_eq!(offers(&context)[0], "band");
        context.observe("sentence.", false);
        assert_eq!(context.boost("bank"), 1.0);
        assert_eq!(offers(&context)[0], "band");
        assert!(
            context.pairs.is_empty(),
            "public context needs no personal learning"
        );
    }

    #[test]
    fn additional_measured_pairs_rank_explicit_offers_without_changing_automatic_spelling() {
        // "worry about" is measured eight times but falls outside the capped
        // public phrase list. Equal edit costs isolate its new ranking evidence.
        let d = dict(&["above", "about"]);
        let f = freq(&[("above", 100), ("about", 180)]);
        let mut context = PhraseContext::default();
        context.observe("worry", false);
        assert_eq!(phrase_boost("worry", "about"), 1.0);
        assert_eq!(context.boost("about"), 1.0);
        assert_eq!(context.offer_boost("about"), 4.0);
        assert_eq!(completions_in_context("abo", d, f, None)[0], "above");
        assert_eq!(
            completions_in_context("abo", d, f, Some(&context))[0],
            "about"
        );
        assert_eq!(
            crate::spell::correction_suggestions("abovt", d, f, None)[0],
            "above"
        );
        assert_eq!(
            crate::spell::correction_suggestions("abovt", d, f, Some(&context))[0],
            "about"
        );
        assert_eq!(
            crate::spell::suggestions("abovt", d, f, Some(&context))[0],
            "about"
        );
        let automatic = crate::spell::assess("abovt", d, f, None);
        let contextual = crate::spell::assess("abovt", d, f, Some(&context));
        assert_eq!(automatic.correction, contextual.correction);
        assert_eq!(automatic.lead, contextual.lead);
        let original = automatic.best.unwrap();
        let unchanged = contextual.best.unwrap();
        assert_eq!(original.text, unchanged.text);
        assert_eq!(original.score, unchanged.score);
        assert!(context.pairs.is_empty());

        context.observe("שלום", false);
        assert_eq!(context.offer_boost("about"), 1.0);
        context.observe("worry", false);
        assert_eq!(context.offer_boost("about"), 4.0);
        context.observe("word.", false);
        assert_eq!(context.offer_boost("about"), 1.0);
        context.observe("worry", false);
        context.clear();
        assert_eq!(context.offer_boost("about"), 1.0);
        assert_eq!(
            completions_in_context("abo", d, f, Some(&context))[0],
            "above"
        );
    }

    #[test]
    fn newly_covered_pair_changes_completion_with_embedded_word_lists() {
        let mut context = PhraseContext::default();
        context.observe("task", false);
        let dictionary = crate::dictionary::en_dict();
        let frequency = crate::dictionary::en_freq();
        assert_eq!(phrase_boost("task", "force"), 1.0);
        assert_eq!(
            completions_in_context("for", dictionary, frequency, None)[0],
            "forget"
        );
        assert_eq!(
            completions_in_context("for", dictionary, frequency, Some(&context))[0],
            "force"
        );
        assert_eq!(context.automatic_discount("force"), 0.0);
        assert!(context.pairs.is_empty());
    }

    #[test]
    fn offer_priors_combine_overlaps_once_and_preserve_longer_history() {
        let mut context = PhraseContext::default();
        for _ in 0..4 {
            context.observe("good", true);
            context.observe("morning", true);
        }
        context.observe("good", true);
        assert_eq!(context.offer_boost("morning"), 4.0);
        context.clear();
        for _ in 0..4 {
            context.observe("worry", true);
            context.observe("about", true);
        }
        context.observe("worry", true);
        assert_eq!(context.boost("about"), 3.0);
        assert_eq!(context.offer_boost("about"), 4.0);
        context.forget_learning();
        assert_eq!(context.boost("about"), 1.0);
        assert_eq!(context.offer_boost("about"), 4.0);

        let model = "about\t20\t1000\nnever\t100\t1000\nnever worry\t100\t100\nnever worry about\t12\t100\nworry\t100\t1000\nworry about\t12\t100\n";
        let mut context = PhraseContext::with_model(model);
        for word in ["never", "worry"] {
            context.observe(word, false);
        }
        assert!(context.boost("about") > 1.0);
        assert_eq!(context.offer_boost("about"), 4.0 * context.boost("about"));
        assert!(context.pairs.is_empty());
    }

    #[test]
    fn measured_phrases_are_attested_bounded_and_work_without_learning() {
        let mut seen = HashSet::new();
        for line in include_str!("phrase_counts.tsv")
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            let fields: Vec<_> = line.split('\t').collect();
            assert_eq!(fields.len(), 6);
            assert!(seen.insert((fields[0], fields[1])));
            let weight = fields[2].parse::<f64>().unwrap();
            let count = fields[3].parse::<u32>().unwrap();
            let previous = fields[4].parse::<u32>().unwrap();
            let next = fields[5].parse::<u32>().unwrap();
            assert!((2.0..=4.0).contains(&weight));
            assert!(count >= 5 && count <= previous && count <= next);
            assert!(count as f64 / previous as f64 >= 0.05);
            assert!(fields[..2]
                .iter()
                .all(|word| (2..=20).contains(&word.chars().count())
                    && (word.chars().all(|c| c.is_ascii_lowercase())
                        || word.chars().all(|c| ('א'..='ת').contains(&c)))));
        }
        assert!(seen.len() > 500);
        let mut context = PhraseContext::default();
        context.observe("prime", false);
        assert_eq!(context.boost("minister"), 4.0);
        let dict = dict(&["minimal", "minister"]);
        let freq = freq(&[("minimal", 100), ("minister", 180)]);
        assert_eq!(
            completions_in_context("min", dict, freq, None)[0],
            "minimal"
        );
        assert_eq!(
            completions_in_context("min", dict, freq, Some(&context))[0],
            "minister"
        );
        context.observe("ראש", false);
        assert_eq!(context.boost("הממשלה"), 4.0);
        assert!(context.pairs.is_empty());
        context.clear();
        assert_eq!(context.boost("הממשלה"), 1.0);
    }

    #[test]
    fn strong_typo_matches_compete_with_weak_exact_matches() {
        let d = dict(&["keyvine", "keyboard"]);
        let f = freq(&[("keyvine", 10_000), ("keyboard", 100)]);
        assert_eq!(offers("keyv", d, f), ["keyboard", "keyvine"]);
        let f = freq(&[("keyvine", 100), ("keyboard", 100)]);
        assert_eq!(
            offers("keyv", d, f).first().map(String::as_str),
            Some("keyvine")
        );
    }

    #[test]
    fn later_slots_account_for_all_cycle_taps() {
        let d = dict(&["abcde", "abcdefg", "abcdefghijk"]);
        let f = freq(&[("abcde", 10), ("abcdefg", 40), ("abcdefghijk", 100)]);
        assert_eq!(offers("abc", d, f), ["abcde", "abcdefg", "abcdefghijk"]);
    }

    #[test]
    fn repeated_phrase_evidence_breaks_completion_ambiguity() {
        let d = dict(&["keyboard", "keyboards"]);
        let f = freq(&[("keyboard", 100), ("keyboards", 180)]);
        let mut context = PhraseContext::default();
        let choices = |context: &PhraseContext| {
            completions_from(
                "keyb",
                d,
                f,
                3,
                30_000,
                &[],
                Ranking {
                    context: Some(context),
                    ..Ranking::default()
                },
            )
        };
        for word in ["my", "keyboards", "my"] {
            context.retain(word);
        }
        assert_eq!(
            choices(&context)[0],
            "keyboard",
            "one observation is insufficient"
        );
        for word in ["keyboards", "my"] {
            context.retain(word);
        }
        assert_eq!(choices(&context)[0], "keyboards");
        context.retain("period.");
        assert_eq!(
            choices(&context)[0],
            "keyboard",
            "punctuation ends the phrase"
        );
        context.retain("my");
        context.clear();
        context.retain("my");
        assert_eq!(
            choices(&context)[0],
            "keyboard",
            "reset forgets pair counts too"
        );
        for word in ["שלום", "עולם", "שלום", "עולם", "שלום"] {
            context.retain(word);
        }
        assert_eq!(context.boost("עולם"), 2.0);
    }

    #[test]
    fn phrase_memory_and_weights_are_bounded() {
        let mut context = PhraseContext::default();
        for _ in 0..100_000 {
            context.retain("my");
            context.retain("keyboard");
        }
        context.retain("my");
        assert_eq!(context.boost("keyboard"), 3.0);
        for a in 'a'..='z' {
            for b in 'a'..='z' {
                context.retain(&format!("word{a}{b}"));
            }
        }
        assert_eq!(context.pairs.len(), 256);
    }

    #[test]
    fn a_completion_must_be_a_dictionary_word() {
        // The frequency list is corpus-derived and full of junk tokens; a
        // completion has to be a word, not merely something people have typed.
        let d = dict(&["helmet"]);
        let f = freq(&[("helo", 100), ("helmet", 9_000)]);
        assert_eq!(finish("hel", d, f).as_deref(), Some("helmet"));
    }

    #[test]
    fn configured_completion_rank_limit_is_respected() {
        let d = dict(&["helot"]);
        let f = freq(&[("helot", 45_000)]);
        assert!(completions_with("hel", d, f, 3, 30_000).is_empty());
        assert_eq!(finish("hel", d, f).as_deref(), Some("helot"));
    }

    #[test]
    fn short_explicit_prefixes_complete_exactly_with_broader_rank_coverage() {
        let d = dict(&["helium", "שלום"]);
        let f = freq(&[("helium", 80_000), ("שלום", 80_000)]);
        for (prefix, word, typo) in [("he", "helium", "hx"), ("של", "שלום", "שז")] {
            assert_eq!(finish(prefix, d, f).as_deref(), Some(word));
            assert!(offers(typo, d, f).is_empty(), "two-letter typo {typo}");
            assert!(completions_with(prefix, d, f, 3, 100_000).is_empty());
            assert!(completions_with(prefix, d, f, 2, 75_000).is_empty());
        }
    }

    #[test]
    fn long_prefixes_recover_two_errors_in_both_languages() {
        let d = dict(&["keyboard", "אוניברסיטה"]);
        let f = freq(&[("keyboard", 1_000), ("אוניברסיטה", 1_000)]);
        for (prefix, word) in [
            ("keybaot", "keyboard"),
            ("keybbax", "keyboard"),
            ("keyord", "keyboard"),
            ("keybxxoard", "keyboard"),
            ("אוניבסוט", "אוניברסיטה"),
        ] {
            let typed: Vec<_> = prefix.chars().collect();
            assert!(within_two_prefix_edits(&typed, word), "{prefix} / {word}");
            if word.chars().count() > typed.len() {
                assert!(
                    offers(prefix, d, f).iter().any(|offer| offer == word),
                    "{prefix}"
                );
            } else {
                assert!(offers(prefix, d, f).is_empty(), "must lengthen {prefix}");
            }
        }
        for prefix in [
            "keyxxxx",
            "kexbxx",
            "xeybaor",
            "keyxx",
            "אוניבxxx",
            "אוניבסוט1",
        ] {
            assert!(offers(prefix, d, f).is_empty(), "{prefix}");
        }
        // Very short candidate words and the longest edited prefix stay bounded.
        assert!(!within_two_prefix_edits(
            &['a'; MAX_EDITED_PREFIX_LEN],
            "ab"
        ));
        assert!(within_two_prefix_edits(
            &"abcdefghijklmnopqrxx".chars().collect::<Vec<_>>(),
            "abcdefghijklmnopqrstuv"
        ));
    }

    #[test]
    fn two_error_completion_preserves_exact_ranking_and_saved_vocabulary() {
        let d = dict(&["keybaotter", "keyboard"]);
        let f = freq(&[("keybaotter", 500), ("keyboard", 1_000)]);
        assert_eq!(offers("keybaot", d, f), ["keybaotter", "keyboard"]);
        let saved = vec!["keyboard".to_owned()];
        assert_eq!(
            completions_from(
                "keybaot",
                dict(&[]),
                Freq::EMPTY,
                2,
                100_000,
                &saved,
                Ranking::default()
            ),
            ["keyboard"]
        );
        assert!(!completions_with("keybaot", d, f, 2, 400)
            .iter()
            .any(|word| word == "keyboard"));
    }

    #[test]
    fn never_completes_a_word_to_itself() {
        let d = dict(&["help"]);
        let f = freq(&[("help", 140)]);
        assert_eq!(finish("help", d, f), None);
    }

    #[test]
    fn a_prefix_with_no_common_word_is_left_alone() {
        let d = dict(&["hello"]);
        let f = freq(&[("hello", 500)]);
        assert_eq!(finish("zqx", d, f), None);
    }

    #[test]
    fn short_and_non_alphabetic_prefixes_are_skipped() {
        let d = dict(&["hello", "the"]);
        let f = freq(&[("hello", 500), ("the", 0)]);
        // One letter matches thousands of words; completing it is a coin flip.
        assert_eq!(finish("t", d, f), None);
        // Digits mean an identifier, not a word being reached for.
        assert_eq!(finish("hel2", d, f), None);
    }

    #[test]
    fn candidates_come_back_in_offer_order_for_the_cycle() {
        let d = dict(&["help", "hello", "helmet", "helicopter", "helpless"]);
        let f = freq(&[
            ("help", 140),
            ("hello", 500),
            ("helmet", 9_000),
            ("helicopter", 12_000),
            ("helpless", 25_000),
        ]);
        let offers = offers("hel", d, f);
        assert_eq!(offers.first().map(String::as_str), Some("help"));
        assert!(offers.len() <= MAX_CANDIDATES);
        // A tap must never offer the same word twice, or the cycle stalls.
        let unique: std::collections::HashSet<&String> = offers.iter().collect();
        assert_eq!(unique.len(), offers.len());
    }

    #[test]
    fn productive_hebrew_completions_use_supported_exact_prefixes() {
        let d = dict(&["מקלדת", "מחשבים", "תוכנה", "מערכות", "וכשמקלחת"]);
        let f = freq(&[
            ("מקלדת", 500),
            ("מחשבים", 1_000),
            ("תוכנה", 2_000),
            ("מערכות", 3_000),
            ("וכשמקלחת", 20_000),
            ("מתיכוון", 50),
        ]);
        for (prefix, expected) in [
            ("וכשמקל", "וכשמקלדת"),
            ("במחש", "במחשבים"),
            ("מהתוכ", "מהתוכנה"),
            ("כשמער", "כשמערכות"),
            ("ולמקל", "ולמקלדת"),
            ("לכשהמקל", "לכשהמקלדת"),
        ] {
            let all = completions_from(
                prefix,
                d,
                f,
                3,
                30_000,
                &[],
                Ranking {
                    all: true,
                    ..Ranking::default()
                },
            );
            assert!(
                all.iter().any(|word| word == expected),
                "missing {expected}: {all:?}"
            );
            assert_eq!(all.iter().filter(|word| *word == expected).count(), 1);
        }
        let all = completions_from(
            "וכשמקל",
            d,
            f,
            3,
            30_000,
            &[],
            Ranking {
                all: true,
                ..Ranking::default()
            },
        );
        assert_eq!(
            all[0], "וכשמקלחת",
            "direct evidence precedes inferred forms"
        );
        for prefix in ["הבמקל", "ווומקל", "במתי"] {
            assert!(
                completions_from(prefix, d, f, 3, 30_000, &[], Ranking::default()).is_empty(),
                "inferred an unsupported form for {prefix}"
            );
        }
        let saved = vec!["מקלדית".to_owned()];
        assert!(completions_from(
            "ובמקל",
            d,
            f,
            3,
            30_000,
            &saved,
            Ranking {
                all: true,
                ..Ranking::default()
            }
        )
        .contains(&"ובמקלדית".to_owned()));
    }

    #[test]
    fn productive_hebrew_forms_are_reachable_across_ranked_vocabulary() {
        let d = crate::dictionary::he_dict();
        let f = crate::dictionary::he_freq();
        let mut stems = Vec::new();
        f.for_each_with_prefix("", |word, rank| {
            if rank <= 30_000
                && (4..=14).contains(&word.chars().count())
                && word.chars().all(|c| ('א'..='ת').contains(&c))
                && d.contains(word)
            {
                stems.push(word.to_owned());
            }
        });
        let mut checked = 0;
        for stem in stems.iter().step_by(211) {
            for attached in ["ב", "ל", "ש", "וב", "מה", "כשה", "ושל", "וכשמ", "לכשה"]
            {
                let expected = format!("{attached}{stem}");
                if d.contains(&expected) {
                    continue;
                }
                let prefix = format!(
                    "{attached}{}",
                    stem.chars()
                        .take(stem.chars().count() - 1)
                        .collect::<String>()
                );
                let all = completions_from(
                    &prefix,
                    d,
                    f,
                    3,
                    30_000,
                    &[],
                    Ranking {
                        all: true,
                        ..Ranking::default()
                    },
                );
                assert!(
                    all.iter().any(|word| word == &expected),
                    "missing {expected} for {prefix}"
                );
                checked += 1;
            }
        }
        assert!(checked >= 200, "only {checked} unlisted forms checked");
        eprintln!(
            "Hebrew completion coverage: {checked} sampled unlisted productive forms reachable."
        );
    }

    #[test]
    fn full_completion_pool_keeps_every_ranked_choice_and_first_page_order() {
        let words = [
            "prefixa",
            "prefixab",
            "prefixabc",
            "prefixabcd",
            "prefixabcde",
            "prefixabcdef",
            "prefixabcdefg",
            "prefixabcdefgh",
            "prefixabcdefghi",
        ];
        let d = dict(&words);
        let entries: Vec<_> = words
            .iter()
            .enumerate()
            .map(|(rank, &word)| (word, rank as u32 * 100))
            .collect();
        let f = freq(&entries);
        let first = offers("pref", d, f);
        let all = completions_from(
            "pref",
            d,
            f,
            3,
            30_000,
            &[],
            Ranking {
                all: true,
                ..Ranking::default()
            },
        );
        assert_eq!(&all[..MAX_CANDIDATES], first);
        assert_eq!(all.len(), words.len());
        for word in words {
            assert_eq!(all.iter().filter(|offer| *offer == word).count(), 1);
        }
        for _ in 0..8 {
            assert_eq!(
                all,
                completions_from(
                    "pref",
                    d,
                    f,
                    3,
                    30_000,
                    &[],
                    Ranking {
                        all: true,
                        ..Ranking::default()
                    },
                )
            );
        }
    }

    #[test]
    fn ranked_vocabulary_is_reachable_across_sampled_prefix_families() {
        let mut checked = 0;
        let mut recovered = 0;
        for (dict, freq, english) in [
            (
                crate::dictionary::en_dict(),
                crate::dictionary::en_freq(),
                true,
            ),
            (
                crate::dictionary::he_dict(),
                crate::dictionary::he_freq(),
                false,
            ),
        ] {
            let mut words = Vec::new();
            freq.for_each_with_prefix("", |word, rank| {
                let len = word.chars().count();
                if rank <= 30_000
                    && (4..=20).contains(&len)
                    && dict.contains(word)
                    && word.chars().all(|c| {
                        if english {
                            c.is_ascii_lowercase()
                        } else {
                            ('א'..='ת').contains(&c)
                        }
                    })
                {
                    words.push(word.to_owned());
                }
            });
            for word in words.iter().step_by(97) {
                let len = word.chars().count();
                for prefix_len in [3, (len / 2).max(3), len - 1] {
                    let prefix: String = word.chars().take(prefix_len).collect();
                    let all = completions_from(
                        &prefix,
                        dict,
                        freq,
                        3,
                        30_000,
                        &[],
                        Ranking {
                            all: true,
                            ..Ranking::default()
                        },
                    );
                    assert!(all.contains(word), "unreachable {word:?} from {prefix:?}");
                    recovered +=
                        usize::from(!all.iter().take(MAX_CANDIDATES).any(|offer| offer == word));
                    checked += 1;
                }
            }
        }
        assert!(checked > 1_000, "coverage unexpectedly small: {checked}");
        assert!(
            recovered > 100,
            "later pages must recover broad vocabulary: {recovered}"
        );
        eprintln!("Completion coverage: {checked} prefix/word pairs reachable; {recovered} require later pages.");
        let d = dict(&["electroencephalographically"]);
        assert_eq!(
            offers("electroencephalographical", d, Freq::EMPTY),
            ["electroencephalographically"]
        );
    }

    #[test]
    fn rare_dictionary_fallback_requires_a_long_exact_specific_prefix() {
        for (prefix, words, near, typo, too_short) in [
            (
                "specific",
                ["specificity", "specifically", "specifics"],
                "specif",
                "specifx",
                "spe",
            ),
            (
                "משמעות",
                ["משמעותיות", "משמעותיים", "משמעותית"],
                "משמעו",
                "משמוע",
                "משמ",
            ),
        ] {
            let d = dict(&words);
            let f = freq(&[(words[2], 45_000)]);
            let all = |typed: &str| {
                completions_from(
                    typed,
                    d,
                    f,
                    3,
                    30_000,
                    &[],
                    Ranking {
                        all: true,
                        ..Ranking::default()
                    },
                )
            };
            let actual = all(prefix);
            for word in words {
                assert!(
                    actual.iter().any(|offer| offer == word),
                    "{prefix}: {actual:?}"
                );
            }
            assert!(!all(near).is_empty(), "{near}");
            assert!(all(typo).is_empty(), "rare fallback edited {typo}");
            assert!(
                all(too_short).is_empty(),
                "rare fallback accepted {too_short}"
            );
            for noisy in [
                format!("{prefix}1"),
                format!("{prefix}_"),
                format!("{prefix}/"),
            ] {
                assert!(all(&noisy).is_empty(), "{noisy}");
            }
        }
        let d = dict(&["abcdeabcdefghijklmn", "abcdef", "abcdegh"]);
        let f = freq(&[("abcdef", 29_999), ("abcdegh", 45_000)]);
        assert_eq!(offers("abcde", d, f), ["abcdef", "abcdegh"]);
        assert!(offers("abcd", d, Freq::EMPTY).is_empty());
    }

    #[test]
    fn a_longer_completion_beats_an_equally_common_short_one() {
        // Equal frequency still gives a bounded advantage to useful savings;
        // this does not remove the short candidate's frequency weight.
        let d = dict(&["tomorrow", "tome"]);
        let f = freq(&[("tomorrow", 900), ("tome", 900)]);
        assert_eq!(finish("tom", d, f).as_deref(), Some("tomorrow"));
    }

    #[test]
    fn frequency_still_dominates_a_lopsided_pair() {
        // Four extra letters do not buy a word that nobody types: `tomorrow` is
        // an order of magnitude commoner, so it wins despite saving less.
        let d = dict(&["tomorrow", "tomographies"]);
        let f = freq(&[("tomorrow", 900), ("tomographies", 29_000)]);
        assert_eq!(finish("tomo", d, f).as_deref(), Some("tomorrow"));
    }

    #[test]
    fn undone_words_are_left_alone_for_the_session() {
        assert!(!suppressed("hostname"));
        suppress("Hostname");
        // Folded, because the buffer's reading is always lowercase.
        assert!(suppressed("hostname"));
        assert!(!suppressed("hostnames"));
    }

    #[test]
    fn unlisting_puts_a_word_back_in_play() {
        // The other half of the toggle: what one double-tap retired, the next
        // one on the same word un-retires.
        suppress("postgres");
        assert!(suppressed("postgres"));
        unlist("Postgres");
        assert!(!suppressed("postgres"));
    }

    #[test]
    fn rewriting_the_ignore_file_touches_only_the_listed_word() {
        let before = "# my words\nhostname\n\n  Postgres  \nkubectl\n";
        let after = without_word(before, "postgres");
        assert_eq!(after, "# my words\nhostname\n\nkubectl\n");

        // Comments are copied through even when they read like the word …
        assert_eq!(
            without_word("# postgres\nfoo\n", "postgres"),
            "# postgres\nfoo\n"
        );
        // … and a word that is not there leaves the file byte-identical.
        assert_eq!(without_word(before, "redis"), before);
    }

    #[cfg(unix)]
    #[test]
    fn ignore_rewrite_preserves_managed_and_temporary_symlinks() {
        use std::os::unix::fs::symlink;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "recast-ignore-symlinks-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ignore.txt");
        let target = dir.join("managed.txt");
        let before = "# managed words\npostgres\nhostname\n";
        let after = "# managed words\nhostname\n";

        std::fs::write(&path, before).unwrap();
        remove_from_ignore_path(&path, "postgres");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after);

        std::fs::remove_file(&path).unwrap();
        std::fs::write(&target, before).unwrap();
        symlink(&target, &path).unwrap();
        remove_from_ignore_path(&path, "postgres");
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), before);

        // A stale temporary link must never redirect the replacement write.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, before).unwrap();
        let unrelated = dir.join("unrelated.txt");
        std::fs::write(&unrelated, "keep this file\n").unwrap();
        let old_temp = path.with_extension("txt.tmp");
        symlink(&unrelated, &old_temp).unwrap();
        remove_from_ignore_path(&path, "postgres");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after);
        assert_eq!(
            std::fs::read_to_string(&unrelated).unwrap(),
            "keep this file\n"
        );
        assert!(std::fs::symlink_metadata(&old_temp)
            .unwrap()
            .file_type()
            .is_symlink());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn listing_a_word_never_joins_it_to_the_line_before() {
        assert_eq!(appended_line("", "hostname"), "hostname\n");
        assert_eq!(appended_line("kubectl\n", "hostname"), "hostname\n");
        // A last line with no newline of its own gets one first, or both
        // entries would be lost to `kubectlhostname`.
        assert_eq!(appended_line("kubectl", "hostname"), "\nhostname\n");
    }

    #[test]
    fn parses_the_abbreviation_file() {
        let table = parse_abbreviations(
            "# my shortcuts\n\
             btw = by the way\n\
             \n\
             TY\tthank you\n\
             addr=1 Main Street, Tel Aviv\n\
             broken line with no separator\n\
             empty =\n",
        );
        assert_eq!(table.get("btw").map(String::as_str), Some("by the way"));
        // Keys are folded to lowercase to match the (lowercase) key buffer …
        assert_eq!(table.get("ty").map(String::as_str), Some("thank you"));
        // … while the expansion keeps exactly what was written.
        assert_eq!(
            table.get("addr").map(String::as_str),
            Some("1 Main Street, Tel Aviv")
        );
        assert!(!table.contains_key("empty"), "a valueless line is skipped");
        assert_eq!(table.len(), 3, "comments and junk lines are skipped");
    }

    #[test]
    fn parses_the_ignore_list() {
        let list = parse_ignore_list("# jargon\nhostname\n\n  Postgres  \n");
        assert!(list.contains("hostname"));
        assert!(list.contains("postgres"), "trimmed and lowercased");
        assert_eq!(list.len(), 2);
    }
}

/// Against the real embedded lists, the way `spell::real_data` is: the unit
/// tests above pin the *rules*, these pin what the rules actually do to the
/// data we ship. A threshold change that looks harmless in isolation shows up
/// here.
#[cfg(test)]
mod real_data {
    use super::*;
    use crate::dictionary::{en_dict, en_freq};

    fn offers(prefix: &str) -> Vec<String> {
        completions_with(
            prefix,
            en_dict(),
            en_freq(),
            crate::config::DEFAULT_COMPLETE_MIN_LEN,
            crate::config::DEFAULT_COMPLETE_MAX_RANK,
        )
    }

    #[test]
    fn finishes_everyday_words() {
        assert_eq!(offers("hel").first().map(String::as_str), Some("help"));
        assert_eq!(offers("typ").first().map(String::as_str), Some("type"));
        let hebrew = completions_with(
            "שלו",
            crate::dictionary::he_dict(),
            crate::dictionary::he_freq(),
            3,
            30_000,
        );
        assert_eq!(hebrew.first().map(String::as_str), Some("שלום"));
        assert_eq!(offers("tomo").first().map(String::as_str), Some("tomorrow"));
        assert_eq!(
            offers("gove").first().map(String::as_str),
            Some("government")
        );
        assert_eq!(
            offers("unde").first().map(String::as_str),
            Some("understand")
        );
        assert_eq!(
            offers("recei").first().map(String::as_str),
            Some("received")
        );
    }

    #[test]
    fn a_crowded_prefix_offers_a_cycle_worth_of_guesses() {
        // The point of the cycle: `hel` is genuinely ambiguous, so the first
        // guess being wrong has to be cheap rather than unlikely.
        let offers = offers("hel");
        assert_eq!(offers.len(), MAX_CANDIDATES);
        for word in ["hello", "helping"] {
            assert!(
                offers.iter().any(|w| w == word),
                "{word} missing: {offers:?}"
            );
        }
    }

    #[test]
    fn curated_technical_vocabulary_is_reachable_without_frequency_entries() {
        let mut checked = 0;
        technical_words().for_each_with_prefix("", |word| {
            if word.len() <= 3 || !word.bytes().all(|c| c.is_ascii_lowercase()) {
                return;
            }
            let prefix = &word[..3];
            let all = completions_from(
                prefix,
                en_dict(),
                en_freq(),
                3,
                30_000,
                &[],
                Ranking {
                    all: true,
                    ..Ranking::default()
                },
            );
            assert!(all.iter().any(|offer| offer == word), "{prefix}: {word}");
            checked += 1;
        });
        assert!(checked > 150, "curated coverage shrank: {checked}");
    }

    #[test]
    fn full_pool_keeps_bounded_ranking_in_both_languages_and_recovers_hidden_choices() {
        for (d, f, prefixes) in [
            (
                en_dict(),
                en_freq(),
                ["hel", "com", "unde", "differ", "devel"],
            ),
            (
                crate::dictionary::he_dict(),
                crate::dictionary::he_freq(),
                ["שלו", "אמר", "מאחו", "משמעות", "פוליטי"],
            ),
        ] {
            let mut recovered = 0;
            for prefix in prefixes {
                let bounded = completions_with(prefix, d, f, 3, 30_000);
                let all = completions_from(
                    prefix,
                    d,
                    f,
                    3,
                    30_000,
                    &[],
                    Ranking {
                        all: true,
                        ..Ranking::default()
                    },
                );
                assert!(all.starts_with(&bounded), "{prefix}: {all:?}");
                assert!(all.len() > bounded.len(), "{prefix}");
                let unique: HashSet<_> = all.iter().collect();
                assert_eq!(unique.len(), all.len(), "{prefix}");
                f.for_each_with_prefix(prefix, |word, rank| {
                    if rank <= 30_000
                        && word.chars().count() > prefix.chars().count()
                        && d.contains(word)
                    {
                        assert!(all.iter().any(|offer| offer == word), "{prefix}: {word}");
                    }
                });
                recovered += all.len() - bounded.len();
            }
            assert!(recovered > 10);
        }
    }

    #[test]
    fn every_offer_is_longer_than_what_was_typed() {
        // A candidate that saves nothing is worse than no candidate: it costs
        // the tap and hands back the same word.
        for prefix in ["hel", "com", "dev", "imp", "thr", "abo"] {
            for word in offers(prefix) {
                assert!(word.len() > prefix.len(), "{prefix} -> {word}");
                assert!(
                    word.starts_with(prefix)
                        || super::edited_prefixes(prefix, true)
                            .iter()
                            .any(|p| word.starts_with(p)),
                    "{prefix} -> {word}"
                );
            }
        }
    }

    #[test]
    fn gibberish_and_identifiers_are_left_alone() {
        assert!(offers("zqxj").is_empty());
        // A prefix more than one edit from any word must still decline.
        assert!(offers("qwrt").is_empty());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod watch_tests {
    use super::*;
    use nix::sys::inotify::{InitFlags, Inotify};

    /// The flag set in `watch_forever` is the whole design decision there, and
    /// getting it wrong fails silently — the watcher runs, blocks, and simply
    /// never notices a save. The two cases below are the two ways editors
    /// actually write a file, and both have to land.
    #[test]
    fn both_kinds_of_save_are_noticed() {
        let dir = std::env::temp_dir().join(format!("recast-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        let inotify = Inotify::init(InitFlags::empty()).expect("inotify");
        inotify.add_watch(&dir, watch_flags()).expect("watch");

        let named = |events: Vec<nix::sys::inotify::InotifyEvent>| -> Vec<String> {
            events
                .iter()
                .filter_map(|e| e.name.as_ref()?.to_str().map(str::to_owned))
                .collect()
        };

        // 1. Saved in place — what `echo >>` and most simple editors do.
        std::fs::write(dir.join("abbrev.txt"), "btw = by the way\n").expect("write");
        let seen = named(inotify.read_events().expect("events"));
        assert!(
            seen.iter().any(|n| n == "abbrev.txt"),
            "an in-place save went unnoticed: {seen:?}"
        );

        // 2. Written elsewhere and renamed over the target — what vim, emacs
        //    and every "atomic save" does. This is the case a watch on the
        //    *file* would miss, because the inode it was watching is gone.
        let tmp = dir.join(".abbrev.txt.swp");
        std::fs::write(&tmp, "btw = by the way\nomw = on my way\n").expect("write tmp");
        let _ = inotify
            .read_events()
            .expect("drain the temp file's own events");
        std::fs::rename(&tmp, dir.join("abbrev.txt")).expect("rename over");
        let seen = named(inotify.read_events().expect("events"));
        assert!(
            seen.iter().any(|n| n == "abbrev.txt"),
            "a rename-over save went unnoticed: {seen:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_nameless_ignored_event_retires_the_watch() {
        let dir = std::env::temp_dir().join(format!("recast-ignored-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let inotify = Inotify::init(InitFlags::IN_NONBLOCK).unwrap();
        let watch = inotify.add_watch(&dir, watch_flags()).unwrap();
        inotify.rm_watch(watch).unwrap();
        assert_eq!(watch_once(&inotify).unwrap(), WatchEvent::Lost);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replacement_directories_reload_existing_lists_and_keep_polling() {
        // Reloading process-global tables must not interfere with other tests.
        const CHILD: &str = "RECAST_DIRECTORY_WATCH_TEST";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "complete::watch_tests::replacement_directories_reload_existing_lists_and_keep_polling",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let dir = config_dir().unwrap();
        let moved = dir.with_extension("moved");
        for move_old in [false, true] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("abbrev.txt"), "btw = old expansion\n").unwrap();
            std::fs::write(dir.join("ignore.txt"), "oldword\n").unwrap();
            reload_user_files();
            assert_eq!(abbreviation("btw").as_deref(), Some("old expansion"));
            assert!(ignored("oldword"));

            let inotify = Inotify::init(InitFlags::IN_NONBLOCK).unwrap();
            inotify.add_watch(&dir, watch_flags()).unwrap();
            if move_old {
                std::fs::rename(&dir, &moved).unwrap();
            } else {
                std::fs::remove_dir_all(&dir).unwrap();
            }
            std::fs::create_dir_all(&dir).unwrap();
            // All edits precede fallback initialization: accepting them as its
            // baseline without reloading would leave the old tables forever.
            std::fs::write(dir.join("abbrev.txt"), "btw = new expansion\n").unwrap();
            std::fs::write(dir.join("ignore.txt"), "newword\n").unwrap();
            assert_eq!(watch_once(&inotify).unwrap(), WatchEvent::Lost);

            let last = poll_once(None);
            assert_eq!(abbreviation("btw").as_deref(), Some("new expansion"));
            assert!(!ignored("oldword"));
            assert!(ignored("newword"));
            std::fs::remove_file(dir.join("ignore.txt")).unwrap();
            let last = poll_once(Some(last));
            assert!(!ignored("newword"));
            std::fs::write(dir.join("ignore.txt"), "restoredword\n").unwrap();
            poll_once(Some(last));
            assert!(ignored("restoredword"));

            std::fs::remove_dir_all(&dir).unwrap();
            if move_old {
                std::fs::remove_dir_all(&moved).unwrap();
            }
        }
    }
}

/// The three lists users can review without finding their configuration folder.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    Ignored,
    Learned,
    Abbreviations,
}

impl RuleKind {
    pub const ALL: [Self; 3] = [Self::Ignored, Self::Learned, Self::Abbreviations];
    pub fn title(self) -> &'static str {
        match self {
            Self::Ignored => "Ignored words",
            Self::Learned => "Learned exceptions",
            Self::Abbreviations => "Abbreviations",
        }
    }
    pub fn hint(self) -> &'static str {
        match self {
            Self::Ignored => "One word per line. Add words to protect them; remove words to allow correction.",
            Self::Learned => "Saved exceptions from repeated undos, one word per line. Remove a word to allow correction, or add one to protect it.",
            Self::Abbreviations => "One shortcut = expansion per line. Example: btw = by the way",
        }
    }
    fn file(self) -> &'static str {
        match self {
            Self::Ignored => "ignore.txt",
            Self::Learned => LEARNED_FILE,
            Self::Abbreviations => "abbrev.txt",
        }
    }
}

pub struct RuleEditor {
    pub kind: RuleKind,
    pub text: String,
    original: String,
}

fn read_rules(path: &std::path::Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("Could not read word rules: {e}")),
    }
}

impl RuleEditor {
    pub fn open(kind: RuleKind) -> Result<Self, String> {
        let path = user_path(kind.file()).ok_or("No configuration directory")?;
        let original = read_rules(&path)?;
        let text = if kind == RuleKind::Learned {
            let mut words: Vec<_> = parse_learned(&original)
                .into_iter()
                .filter(|(_, count)| *count >= LEARNED_MIN)
                .map(|(word, _)| word)
                .collect();
            words.sort();
            words.join("\n")
        } else {
            original.clone()
        };
        Ok(Self {
            kind,
            text,
            original,
        })
    }

    pub fn save(&mut self) -> Result<(), String> {
        // Validate before touching either disk or the live tables. Existing
        // file parsers remain forgiving of hand-written files.
        for (index, line) in self.text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let word = if self.kind == RuleKind::Abbreviations {
                let (key, value) = line
                    .split_once('=')
                    .or_else(|| line.split_once('\t'))
                    .ok_or_else(|| format!("Line {}: use shortcut = expansion", index + 1))?;
                if value.trim().is_empty() || value.chars().any(char::is_control) {
                    return Err(format!("Line {}: enter a single-line expansion", index + 1));
                }
                key.trim()
            } else {
                line
            };
            if word.is_empty() || word.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(format!("Line {}: enter one word without spaces", index + 1));
            }
        }
        let path = user_path(self.kind.file()).ok_or("No configuration directory")?;
        // Hold the same lock as undo learning until its replacement is applied.
        let mut counts = learned_words().lock().map_err(|_| "Word rules are busy")?;
        if read_rules(&path)? != self.original {
            return Err("These rules changed since opening. Copy your edits, then reopen the editor to review the latest rules.".into());
        }
        let words = parse_ignore_list(&self.text);
        let mut next_counts = parse_learned(&self.original);
        let text = if self.kind == RuleKind::Learned {
            next_counts.retain(|word, count| *count < LEARNED_MIN || words.contains(word));
            for word in &words {
                let count = next_counts.entry(word.clone()).or_default();
                *count = (*count).max(LEARNED_MIN);
            }
            learned_text(&next_counts)
        } else {
            self.text.clone()
        };
        crate::settings::write_atomic(&path, &text)
            .map_err(|e| format!("Could not save word rules: {e}"))?;
        match self.kind {
            RuleKind::Ignored => {
                let old = parse_ignore_list(&self.original);
                if let Ok(mut suppressed) = suppressed_words().lock() {
                    for word in old.difference(&words) {
                        suppressed.remove(word);
                    }
                }
                *ignore_list()
                    .lock()
                    .map_err(|_| "Could not apply ignored words")? = words;
            }
            RuleKind::Learned => {
                if let Ok(mut suppressed) = suppressed_words().lock() {
                    for word in counts
                        .keys()
                        .filter(|word| !next_counts.contains_key(*word))
                    {
                        suppressed.remove(word);
                    }
                }
                *counts = next_counts;
            }
            RuleKind::Abbreviations => {
                *abbreviations()
                    .lock()
                    .map_err(|_| "Could not apply abbreviations")? = parse_abbreviations(&text);
            }
        }
        self.original = text;
        Ok(())
    }
}

#[cfg(test)]
mod rule_editor_tests {
    use super::*;

    #[test]
    fn rules_save_apply_validate_and_preserve_concurrent_edits() {
        // Rule tables are process-global; keep these edits out of other tests.
        const CHILD: &str = "RECAST_RULE_EDITOR_TEST";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "complete::rule_editor_tests::rules_save_apply_validate_and_preserve_concurrent_edits"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let mut editor = RuleEditor::open(RuleKind::Ignored).unwrap();
        editor.text = "# Names\nSupino\nשלום\n".into();
        editor.save().unwrap();
        assert!(ignored("supino") && ignored("שלום"));
        suppress("supino");
        editor.text = "# Names\nשלום\n".into();
        editor.save().unwrap();
        assert!(!ignored("supino") && !suppressed("supino"));
        assert_eq!(read_user_file("ignore.txt"), editor.text);
        editor.text = "two words".into();
        assert!(editor.save().unwrap_err().contains("Line 1"));
        assert!(ignored("שלום"));

        let mut editor = RuleEditor::open(RuleKind::Abbreviations).unwrap();
        editor.text = "btw = by the way\nshalom = שלום עולם\n".into();
        editor.save().unwrap();
        assert_eq!(abbreviation("btw").as_deref(), Some("by the way"));
        assert_eq!(abbreviation("shalom").as_deref(), Some("שלום עולם"));
        editor.text = "broken line".into();
        assert!(editor.save().is_err());
        assert_eq!(abbreviation("btw").as_deref(), Some("by the way"));
        editor.text.clear();
        editor.save().unwrap();
        assert!(abbreviation("btw").is_none());
        std::fs::write(user_path("abbrev.txt").unwrap(), "omw = on my way\n").unwrap();
        editor.text = "brb = be right back".into();
        assert!(editor.save().unwrap_err().contains("changed since opening"));
        assert_eq!(read_user_file("abbrev.txt"), "omw = on my way\n");

        suppress("recieve");
        learn("recieve");
        learn("recieve");
        suppress("keyboad");
        learn("keyboad");
        let mut editor = RuleEditor::open(RuleKind::Learned).unwrap();
        assert_eq!(editor.text, "recieve");
        editor.text = "Supino".into();
        editor.save().unwrap();
        assert!(!learned("recieve") && !suppressed("recieve"));
        assert!(learned("supino"));
        assert_eq!(
            parse_learned(&read_user_file(LEARNED_FILE)).get("keyboad"),
            Some(&1)
        );
        // A delayed undo must not reintroduce a removed exception.
        learn("recieve");
        assert!(!learned("recieve"));
        let mut editor = RuleEditor::open(RuleKind::Learned).unwrap();
        learn("keyboad");
        editor.text.clear();
        assert!(editor.save().unwrap_err().contains("changed since opening"));
        assert!(learned("keyboad"));

        // A failed write must leave the live table unchanged.
        let mut editor = RuleEditor::open(RuleKind::Ignored).unwrap();
        let temp = user_path("ignore.txt")
            .unwrap()
            .with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&temp, "occupied").unwrap();
        editor.text.clear();
        assert!(editor.save().is_err());
        assert!(ignored("שלום"));
        std::fs::remove_file(temp).unwrap();
        #[cfg(unix)]
        {
            let path = user_path("ignore.txt").unwrap();
            let target = user_path("managed-ignore.txt").unwrap();
            std::fs::rename(&path, &target).unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(editor.save().unwrap_err().contains("symlink"));
            assert!(ignored("שלום"));
        }
        std::fs::remove_dir_all(config_dir().unwrap()).unwrap();
    }
}
