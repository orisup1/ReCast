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
//!   walk, no index and no allocation per rejected candidate.
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

/// Longest partial word we will try to complete. Past this the user is typing
/// an identifier or a URL, not reaching for a word.
const MAX_PREFIX_LEN: usize = 20;

/// How many guesses a cycle offers before coming back round to what the user
/// typed. Small on purpose: past three or four taps, deleting the word and
/// typing it out is faster than hunting, and every extra candidate is a rarer
/// word than the one before it.
pub const MAX_CANDIDATES: usize = 4;

/// Phrase evidence is local to one uninterrupted typing context, never persisted.
/// Two observations are required before a pair influences completion ranking.
#[derive(Default)]
pub(crate) struct PhraseContext {
    previous: Option<String>,
    pairs: HashMap<(String, String), u16>,
}

impl PhraseContext {
    pub(crate) fn clear(&mut self) {
        self.previous = None;
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
            self.previous = None;
            return;
        }
        if let Some(previous) = self.previous.take().filter(|_| learn) {
            let pair = (previous, word.clone());
            if self.pairs.len() < 256 || self.pairs.contains_key(&pair) {
                let count = self.pairs.entry(pair).or_default();
                *count = count.saturating_add(1);
            }
        }
        self.previous = Some(word);
    }

    pub(crate) fn boost(&self, word: &str) -> f64 {
        let count = self
            .previous
            .as_ref()
            .and_then(|previous| self.pairs.get(&(previous.clone(), word.to_owned())))
            .copied()
            .unwrap_or(0);
        let personal = if count < 2 {
            1.0
        } else {
            1.0 + f64::from(count.min(4)) / 2.0
        };
        let bundled = self
            .previous
            .as_deref()
            .map_or(1.0, |previous| phrase_boost(previous, word));
        personal.max(bundled)
    }
}

/// Hand-curated relative phrase weights, not measured corpus counts. Public
/// language priors work immediately without collecting personal word pairs.
fn phrase_boost(previous: &str, word: &str) -> f64 {
    static PHRASES: OnceLock<HashMap<&'static str, Vec<(&'static str, f64)>>> = OnceLock::new();
    let phrases = PHRASES.get_or_init(|| {
        let mut map: HashMap<_, Vec<_>> = HashMap::new();
        for line in include_str!("phrases.tsv")
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            let fields: Vec<_> = line.split('\t').collect();
            if let [previous, next, weight] = fields.as_slice() {
                if let Ok(weight) = weight.parse::<f64>() {
                    map.entry(*previous).or_default().push((*next, weight));
                }
            }
        }
        map
    });
    phrases
        .get(previous)
        .and_then(|entries| entries.iter().find(|(next, _)| *next == word))
        .map_or(1.0, |(_, weight)| *weight)
}

#[derive(Default)]
struct Ranking<'a> {
    context: Option<&'a PhraseContext>,
    offset: usize,
    exclude: Option<&'a str>,
}

/// Frequency and retained personal evidence estimate likelihood. Prefix edits
/// receive a tenfold penalty. Each cycle slot subtracts all taps needed to reach
/// it from the letters saved; zero-saving offers remain available as fallbacks.
struct Candidate {
    word: String,
    saved: usize,
    weight: f64,
    rank: u32,
}

impl Candidate {
    fn value(&self, taps: usize) -> f64 {
        self.saved.saturating_sub(taps) as f64 * self.weight
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
        },
    ) {
        if !out.contains(&word) {
            out.push(word);
        }
    }
    out
}

/// Core of [`completions`] with the tuning knobs passed in, so tests don't
/// depend on the environment. Abbreviations are not consulted here.
///
/// A candidate must be a *longer* word than what was typed (completing a word
/// to itself is a no-op the caller shouldn't have to unwind), a dictionary word
/// — the frequency list is corpus-derived and full of junk tokens — and common
/// enough to be a word someone reaching for that prefix might mean.
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
    let english = prefix.chars().all(|c| c.is_ascii_lowercase());
    if len < min_len
        || len > MAX_PREFIX_LEN
        || !(english || prefix.chars().all(|c| ('א'..='ת').contains(&c)))
    {
        return Vec::new();
    }
    let mut candidates: HashMap<String, Candidate> = HashMap::new();
    // Exact and one-edit matches compete in the same pool. A typo needs much
    // stronger frequency evidence to beat an equally useful exact match.
    let mut prefixes = edited_prefixes(prefix, english);
    // Editing a three-letter prefix is too ambiguous to compete with an exact
    // offer. Keep short-prefix recovery only when there is no exact candidate.
    if len < 4 {
        let mut exact = false;
        freq.for_each_with_prefix(prefix, |word, rank| {
            exact |= word.chars().count() > len && rank <= max_rank && dict.contains(word);
        });
        exact |= saved
            .iter()
            .any(|word| word.starts_with(prefix) && word.chars().count() > len);
        if exact {
            prefixes.clear();
        }
    }
    prefixes.insert(prefix.to_owned());
    let mut consider = |word: &str, rank: u32, explicit: bool| {
        let word_len = word.chars().count();
        if word_len <= len
            || word_len > crate::types::MAX_WORD_KEYS
            || ranking.exclude == Some(word)
            || candidates.contains_key(word)
            || (!explicit && (rank > max_rank || !dict.contains(word)))
        {
            return;
        }
        let penalty = if word.starts_with(prefix) { 1.0 } else { 0.1 };
        let weight = penalty
            * f64::from(crate::personal::personal_boost(word))
            * ranking.context.map_or(1.0, |context| context.boost(word))
            / (f64::from(rank) + 1.0);
        candidates.insert(
            word.to_owned(),
            Candidate {
                word: word.to_owned(),
                saved: word_len - len,
                weight,
                rank,
            },
        );
    };
    for matching in &prefixes {
        freq.for_each_with_prefix(matching, |word, rank| consider(word, rank, false));
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
            consider(word, freq.rank(word).unwrap_or(1_000), true);
        }
    }
    let mut candidates: Vec<_> = candidates.into_values().collect();
    let mut out = Vec::with_capacity(MAX_CANDIDATES);
    for slot in 0..MAX_CANDIDATES {
        let taps = ranking.offset + slot + 1;
        let Some((index, _)) = candidates.iter().enumerate().max_by(|(_, a), (_, b)| {
            a.value(taps)
                .total_cmp(&b.value(taps))
                .then_with(|| a.weight.total_cmp(&b.weight))
                .then_with(|| b.rank.cmp(&a.rank))
                .then_with(|| b.word.cmp(&a.word))
        }) else {
            break;
        };
        out.push(candidates.swap_remove(index).word);
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
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    // Rename over the original rather than truncating it: the user wrote this
    // file, and a failed write should cost them nothing.
    let tmp = path.with_extension("txt.tmp");
    if std::fs::write(&tmp, without_word(&text, word)).is_ok()
        && std::fs::rename(&tmp, &path).is_err()
    {
        let _ = std::fs::remove_file(&tmp);
    }
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
    *counts.entry(word).or_insert(0) += 1;
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
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let tmp = path.with_extension("txt.tmp");
    if std::fs::write(&tmp, learned_text(counts)).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
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
    use nix::sys::inotify::{AddWatchFlags, InitFlags, Inotify};

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
    // CLOSE_WRITE catches an in-place save, MOVED_TO the rename-over kind,
    // CREATE a first-ever write, DELETE a list emptied by removing the file.
    let flags = AddWatchFlags::IN_CLOSE_WRITE
        | AddWatchFlags::IN_MOVED_TO
        | AddWatchFlags::IN_CREATE
        | AddWatchFlags::IN_DELETE;
    if inotify.add_watch(&dir, flags).is_err() {
        return poll_watch();
    }

    loop {
        // Blocks. No timer, no wakeups, nothing scheduled — the whole point of
        // this over the poll it replaces.
        let Ok(events) = inotify.read_events() else {
            // The watch descriptor is gone (the directory was deleted, or the
            // filesystem does not support inotify after all). Polling still
            // works on whatever replaces it.
            return poll_watch();
        };
        let ours = events.iter().any(|e| {
            e.name
                .as_ref()
                .and_then(|n| n.to_str())
                .is_some_and(|n| WATCHED.contains(&n))
        });
        if ours {
            reload_user_files();
        }
    }
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
    let stamp = || {
        WATCHED.map(|name| {
            user_path(name)
                .and_then(|p| std::fs::metadata(p).ok())
                .and_then(|m| m.modified().ok())
        })
    };
    let mut last = stamp();
    loop {
        std::thread::sleep(WATCH_INTERVAL);
        let now = stamp();
        if now != last {
            last = now;
            reload_user_files();
        }
    }
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
    fn completion_accounts_for_the_acceptance_tap() {
        let d = dict(&["hello", "help", "helmet"]);
        let f = freq(&[("hello", 500), ("help", 140), ("helmet", 9_000)]);
        assert_eq!(finish("hel", d, f).as_deref(), Some("hello"));
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
        assert_eq!(finish("שלו", hebrew, ranks).as_deref(), Some("שלומות"));
        assert_eq!(finish("שול", hebrew, ranks).as_deref(), Some("שלומות"));
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
        assert_eq!(offers("abc", d, f), ["abcde", "abcdefghijk", "abcdefg"]);
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
    fn a_rare_completion_is_not_offered() {
        let d = dict(&["helot"]);
        let f = freq(&[("helot", 45_000)]);
        assert_eq!(finish("hel", d, f), None);
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
        assert_eq!(offers.first().map(String::as_str), Some("hello"));
        assert!(offers.len() <= MAX_CANDIDATES);
        // A tap must never offer the same word twice, or the cycle stalls.
        let unique: std::collections::HashSet<&String> = offers.iter().collect();
        assert_eq!(unique.len(), offers.len());
    }

    #[test]
    fn a_longer_completion_beats_an_equally_common_short_one() {
        // Same frequency, so the tie-break is what the completion is *for*:
        // `tomorrow` saves four keystrokes for the tap, `tomb` saves none worth
        // having. Ranking by frequency alone could not tell these apart.
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
    use nix::sys::inotify::{AddWatchFlags, InitFlags, Inotify};

    /// The flag set in `watch_forever` is the whole design decision there, and
    /// getting it wrong fails silently — the watcher runs, blocks, and simply
    /// never notices a save. The two cases below are the two ways editors
    /// actually write a file, and both have to land.
    #[test]
    fn both_kinds_of_save_are_noticed() {
        let dir = std::env::temp_dir().join(format!("recast-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        let inotify = Inotify::init(InitFlags::empty()).expect("inotify");
        let flags = AddWatchFlags::IN_CLOSE_WRITE
            | AddWatchFlags::IN_MOVED_TO
            | AddWatchFlags::IN_CREATE
            | AddWatchFlags::IN_DELETE;
        inotify.add_watch(&dir, flags).expect("watch");

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
