//! Word lists and the decision core.
//!
//! The four lists (English/Hebrew dictionaries and frequency lists) are
//! embedded in the binary in the *prepared* form `build.rs` writes: folded,
//! deduplicated and **sorted**, one entry per line. Sorted is the whole point —
//! it means a lookup is a binary search straight over the embedded bytes
//! ([`Dict`] / [`Freq`]), so the program never allocates a hash table, never
//! parses anything at startup, and the ~11 MB of word data stays as read-only
//! pages of the executable that the OS can drop under memory pressure rather
//! than ~100 MB of resident heap.

use std::sync::OnceLock;

use crate::layout::switch_layout_to;
use crate::types::Language;
use crate::config::Config;

/// A sorted, `\n`-separated word list living in the binary's read-only data.
///
/// `Copy` and pointer-sized: it is passed around by value, and cloning it costs
/// nothing because there is nothing to clone — the words are never copied out
/// of the executable image.
#[derive(Clone, Copy)]
pub struct Dict {
    blob: &'static str,
}

/// A sorted `word\trank` list (rank 0-based; lower = more common). Used only to
/// break homograph ties and to gate spelling suggestions, never as a membership
/// trigger.
#[derive(Clone, Copy)]
pub struct Freq {
    blob: &'static str,
}

impl Dict {
    pub const fn new(blob: &'static str) -> Self {
        Self { blob }
    }

    /// Exact membership test: a binary search over the sorted lines.
    pub fn contains(self, word: &str) -> bool {
        lookup(self.blob.as_bytes(), word.as_bytes()).is_some()
    }
}

impl Freq {
    pub const fn new(blob: &'static str) -> Self {
        Self { blob }
    }

    /// Rank of `word`, if it is common enough to appear in the list.
    pub fn rank(self, word: &str) -> Option<u32> {
        let line = lookup(self.blob.as_bytes(), word.as_bytes())?;
        let tab = line.iter().position(|&b| b == b'\t')?;
        parse_rank(&line[tab + 1..])
    }
}

/// The key of a blob line: everything before the first tab (a `Freq` line), or
/// the whole line when there is none (a `Dict` line).
fn key_of(line: &[u8]) -> &[u8] {
    match line.iter().position(|&b| b == b'\t') {
        Some(i) => &line[..i],
        None => line,
    }
}

fn parse_rank(bytes: &[u8]) -> Option<u32> {
    let mut rank: u32 = 0;
    for &b in bytes {
        rank = rank.checked_mul(10)?.checked_add(b.checked_sub(b'0')? as u32)?;
    }
    Some(rank)
}

/// Binary search for the line whose key equals `needle`, over a blob whose
/// lines are sorted by byte order.
///
/// The blob carries no index — a probe lands on an arbitrary byte and walks
/// back to the start of the line it fell inside, which is what keeps the whole
/// structure to "just the sorted text" with nothing else resident. `lo` is
/// always the start of a line and `hi` is always a line start or the end of the
/// blob, so each step either moves `lo` past the probed line or pulls `hi` down
/// to it, and the window always shrinks.
fn lookup<'a>(blob: &'a [u8], needle: &[u8]) -> Option<&'a [u8]> {
    let (mut lo, mut hi) = (0usize, blob.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        // Start of the line containing `mid` (never before `lo`, which is
        // itself a line start).
        let start = match blob[lo..mid].iter().rposition(|&b| b == b'\n') {
            Some(i) => lo + i + 1,
            None => lo,
        };
        let end = start
            + blob[start..]
                .iter()
                .position(|&b| b == b'\n')
                .unwrap_or(blob.len() - start);
        let line = &blob[start..end];
        match key_of(line).cmp(needle) {
            std::cmp::Ordering::Less => lo = end + 1,
            std::cmp::Ordering::Greater => hi = start,
            std::cmp::Ordering::Equal => return Some(line),
        }
    }
    None
}

/// The English dictionary (sorted blob, prepared by `build.rs`).
pub const fn en_dict() -> Dict {
    Dict::new(include_str!(concat!(env!("OUT_DIR"), "/en_dict.blob")))
}

/// The Hebrew dictionary (sorted blob, prepared by `build.rs`).
pub const fn he_dict() -> Dict {
    Dict::new(include_str!(concat!(env!("OUT_DIR"), "/he_dict.blob")))
}

/// The English frequency list (sorted blob, prepared by `build.rs`).
pub const fn en_freq() -> Freq {
    Freq::new(include_str!(concat!(env!("OUT_DIR"), "/en_freq.blob")))
}

/// The Hebrew frequency list (sorted blob, prepared by `build.rs`).
pub const fn he_freq() -> Freq {
    Freq::new(include_str!(concat!(env!("OUT_DIR"), "/he_freq.blob")))
}

fn debug_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("RECAST_DEBUG").is_some())
}

/// Missing-space split correction is opt-in. It can only ever fire when the
/// whole buffer is gibberish in both layouts, but even then it cannot reliably
/// tell "one word we simply don't have in the dictionary" from "two words typed
/// without a space, the second in the wrong layout" — so by default it stays off
/// and a single word is never carved up. Set `RECAST_SPLIT=1` to enable it.
fn split_enabled() -> bool {
    // Enable missing‑space split via RECAST_SPLIT env (exposed through Config).
    Config::global().split_enabled
}

/// Frequency rank of `text` in its language's frequency list, if the word is
/// present (i.e. common enough to appear in the top-N list).
fn freq_rank(text: &str, lang: Language, en_freq: Freq, he_freq: Freq) -> Option<u32> {
    match lang {
        Language::English => en_freq.rank(text),
        Language::Hebrew => he_freq.rank(text),
    }
}

// Homograph tie-break tuning. When a key sequence is a real word in *both*
// layouts we normally keep the current layout; we only override that when the
// OTHER reading is decisively the one the user more likely meant.
const FREQ_COMMON_MAX: u32 = 2000; // the "other" reading must rank at least this common
const FREQ_RARER_FACTOR: u32 = 10; // and be >= this many times more common than current

/// Homograph tie-break: both `cur_text` (current layout) and `oth_text` (other
/// layout) are real words. Returns `true` when the other reading is decisively
/// more common and should win. Conservative — it fires only when the other
/// reading is a top-`FREQ_COMMON_MAX` word AND the current reading is either
/// absent from the frequency list or at least `FREQ_RARER_FACTOR`× rarer. With
/// empty frequency lists (as in unit tests) it always returns `false`, so the
/// prior "keep current layout" behaviour is unchanged.
fn other_decisively_more_common(
    cur_text: &str,
    current: Language,
    oth_text: &str,
    other: Language,
    en_freq: Freq,
    he_freq: Freq,
) -> bool {
    if !Config::global().freq_enabled {
        return false;
    }
    let Some(oth_rank) = freq_rank(oth_text, other, en_freq, he_freq) else {
        return false; // the other reading isn't even a common word — don't override.
    };
    if oth_rank > FREQ_COMMON_MAX {
        return false;
    }
    match freq_rank(cur_text, current, en_freq, he_freq) {
        // Current reading is also ranked: switch only if the other is many times more common.
        Some(cur_rank) => cur_rank >= oth_rank.saturating_mul(FREQ_RARER_FACTOR),
        // Current reading is absent from the top-N list while the other is very
        // common: the common reading almost certainly wins.
        None => true,
    }
}

/// One-letter inflectional prefixes that Hebrew attaches to nouns/verbs:
/// ו (and), ה (the), ל (to/for), ב (in), כ (as/like), מ (from), ש (that).
const HE_PREFIXES: &[char] = &['ו', 'ה', 'ל', 'ב', 'כ', 'מ', 'ש'];

/// Hebrew lookup with single-prefix fallback: if the word is not in the dict
/// directly, try stripping a leading prefix letter and looking up the rest.
/// Only one prefix is stripped to avoid over-matching; the dictionary already
/// holds many common prefixed forms as full entries.
fn matches_hebrew(word: &str, dict: Dict) -> bool {
    if dict.contains(word) {
        return true;
    }
    let mut iter = word.chars();
    if let Some(first) = iter.next() {
        if HE_PREFIXES.contains(&first) {
            let rest = iter.as_str();
            if !rest.is_empty() && dict.contains(rest) {
                return true;
            }
        }
    }
    false
}

/// Strict dictionary membership for `lang`. This is the *trigger* test — "these
/// keystrokes are unambiguously a word in the other language, switch to it." It
/// is strict on both sides so a name/typo is never flipped just because its
/// prefix-stripped reading happens to be a Hebrew word.
fn valid_strict(text: &str, lang: Language, en_dict: Dict, he_dict: Dict) -> bool {
    if text.is_empty() {
        return false;
    }
    match lang {
        Language::English => en_dict.contains(text),
        Language::Hebrew => he_dict.contains(text),
    }
}

/// Looser membership for `lang`. This is the *guard* test — "the user already
/// typed a real word in this layout, leave it alone." Hebrew adds the one-letter
/// inflectional-prefix fallback so prefixed real words (absent from the dict
/// directly) still count and are never carved up. English has no such prefixes,
/// so it is identical to the strict check.
fn valid_loose(text: &str, lang: Language, en_dict: Dict, he_dict: Dict) -> bool {
    if text.is_empty() {
        return false;
    }
    match lang {
        Language::English => en_dict.contains(text),
        Language::Hebrew => matches_hebrew(text, he_dict),
    }
}

/// Whole-word decision when the current layout is known. This is the core of the
/// "works like magic" behaviour: the decision is anchored on what the user is
/// *actually* typing in, instead of guessing symmetrically from the keystrokes.
///
/// `text_en` / `text_he` are the same key sequence read under each layout;
/// `current` is the live keyboard layout.
///
///   1. The keystrokes already form a strict word in the **current** layout →
///      trust the user, do nothing. This is the user's own rule from day one:
///      "to change a word it must not mean anything in the current language."
///      It also covers homographs (valid in both layouts), which are left to
///      the layout the user is actually in.
///   2. Else they form a confident (strict) word in the **other** layout →
///      the user typed in the wrong layout, switch. (Fixes the actual
///      mistypes.) Short words (≤3 chars) are collision-prone, so this trigger
///      can be turned off for them via `RECAST_SHORT=0`.
///   3. Otherwise it's an unknown word (name/typo/slang) → leave it alone.
///
/// Note: the guard is deliberately *strict*, not loose. A prefixed Hebrew form
/// whose keys also spell a real English word switches to English — when both
/// readings are plausible the strict dictionary hit wins over the inferred
/// prefix match (see `prefixed_hebrew_with_english_collision_always_switches`).
fn decide_known(
    text_en: &str,
    text_he: &str,
    current: Language,
    en_dict: Dict,
    he_dict: Dict,
    en_freq: Freq,
    he_freq: Freq,
) -> Option<Language> {
    let other = current.other();
    let cur_text = if current == Language::English { text_en } else { text_he };
    let oth_text = if other == Language::English { text_en } else { text_he };

    let oth_strict = valid_strict(oth_text, other, en_dict, he_dict);
    // Short-word gate: ≤3-char words are dictionary-collision-prone; when
    // disabled (RECAST_SHORT=0) an other-layout reading of that length never
    // triggers a switch — neither the plain trigger nor the frequency tie-break.
    let short_block = !Config::global().short_enabled && oth_text.chars().count() <= 3;

    // Guard: the current layout already forms a strict word (including the
    // homograph case where both layouts do) → preserve user intent, unless the
    // other reading is decisively more common (frequency tie-break).
    if valid_strict(cur_text, current, en_dict, he_dict) {
        if oth_strict
            && !short_block
            && other_decisively_more_common(cur_text, current, oth_text, other, en_freq, he_freq)
        {
            return Some(other);
        }
        return None;
    }
    if short_block {
        return None;
    }
    // Trigger: the other layout yields a confident word → switch.
    if oth_strict {
        return Some(other);
    }
    None
}

/// Whole-word decision when the current layout can't be determined. Falls back
/// to a symmetric rule: switch only when exactly one language is a strict word
/// and the other isn't even a loose match — conservative, so it neither mangles
/// nor fires on ambiguous input.
fn decide_unknown(
    text_en: &str,
    text_he: &str,
    en_dict: Dict,
    he_dict: Dict,
    en_freq: Freq,
    he_freq: Freq,
) -> Option<Language> {
    // Short-word gate: when disabled, a ≤3-char reading never counts as a
    // trigger — the same collision guard as in `decide_known`.
    let short_ok = |text: &str| {
        Config::global().short_enabled || text.chars().count() > 3
    };
    let en_strict =
        short_ok(text_en) && valid_strict(text_en, Language::English, en_dict, he_dict);
    let he_strict =
        short_ok(text_he) && valid_strict(text_he, Language::Hebrew, en_dict, he_dict);
    // If exactly one layout has a strict match, switch to that layout.
    if en_strict && !he_strict {
        Some(Language::English)
    } else if he_strict && !en_strict {
        Some(Language::Hebrew)
    } else if en_strict && he_strict {
        // Both layouts read as words: break the tie by frequency, else leave it
        // alone. (Winner must be decisively more common than the loser.)
        if other_decisively_more_common(text_he, Language::Hebrew, text_en, Language::English, en_freq, he_freq) {
            Some(Language::English)
        } else if other_decisively_more_common(text_en, Language::English, text_he, Language::Hebrew, en_freq, he_freq) {
            Some(Language::Hebrew)
        } else {
            None
        }
    } else {
        None
    }
}

/// What the two correction pipelines decided to do with a finished word.
///
/// Exactly one of these ever comes back for a given word: the pipelines are
/// mutually exclusive by construction (see [`plan`]), so a word that gets
/// spell-corrected is never also layout-switched, and vice versa.
#[derive(Clone, Debug, PartialEq)]
pub enum Fix {
    /// Wrong-layout mistype: the layout has already been switched, so the
    /// caller should erase `keys[start..]` and put the corrected word in its
    /// place. `start = 0` is the whole buffer; a non-zero `start` comes from
    /// the missing-space split.
    ///
    /// Two ways to put it back, and platforms pick whichever their injection
    /// API supports: `text` is the finished word (what `keys[start..]` spells
    /// in the *new* layout) for platforms that insert text directly, and
    /// replaying `keys[start..]` produces exactly the same characters now that
    /// the layout has changed, for platforms that can only send keycodes.
    Layout { start: usize, text: String },
    /// Misspelling in the current (English) layout: the layout is untouched and
    /// the caller should erase the whole word and type `text` instead. `text`
    /// is always plain lowercase ASCII, so it is typeable through the English
    /// keymap.
    Spelling { text: String },
}

/// Internal counterpart of [`Fix`] that still carries the target language, so
/// the pure planner can be tested without performing the switch.
#[derive(Clone, Debug, PartialEq)]
enum Plan {
    Switch { lang: Language, start: usize },
    Spell { text: String },
}

fn debug_log(word_en: &str, word_he: &str, target: Option<Language>, switched: bool) {
    if !debug_enabled() {
        return;
    }
    println!("{}", word_en);
    println!("{}", word_he);
    println!(
        "English: {}",
        if matches!(target, Some(Language::English)) { "True" } else { "False" }
    );
    println!(
        "Hebrew: {}",
        if matches!(target, Some(Language::Hebrew)) { "True" } else { "False" }
    );
    println!("Switch: {}", if switched { "True" } else { "False" });
}

/// Pure planning step: decide what to do with the word, given the folded
/// buffers, the per-key offset tables, and the live `current` layout. Kept free
/// of any I/O so it is unit testable; the actual layout switch happens in the
/// caller.
///
/// The two pipelines run in a fixed order and the first one to produce a plan
/// wins — a word is only ever corrected once:
///
///   1. **Layout** (whole buffer, then the opt-in missing-space split). It goes
///      first because it is the *exact* signal: the keystrokes literally spell
///      a real word in the other language, no guessing involved.
///   2. **Spelling** (English only). Reached only when the layout pipeline
///      declined, i.e. the keystrokes are not a word in either language. The
///      resulting word is typed as-is and never re-examined by the layout
///      pipeline, so a spell-corrected word whose keys happen to also read as a
///      Hebrew word is *not* subsequently flipped.
#[allow(clippy::too_many_arguments)]
fn plan(
    full_en: &str,
    full_he: &str,
    offsets_en: &[usize],
    offsets_he: &[usize],
    keys_len: usize,
    current: Option<Language>,
    en_dict: Dict,
    he_dict: Dict,
    en_freq: Freq,
    he_freq: Freq,
) -> Option<Plan> {
    // Whole-buffer decision first — this is what fires for virtually every real
    // correction.
    let whole = match current {
        Some(cur) => decide_known(full_en, full_he, cur, en_dict, he_dict, en_freq, he_freq),
        None => decide_unknown(full_en, full_he, en_dict, he_dict, en_freq, he_freq),
    };
    if let Some(lang) = whole {
        return Some(Plan::Switch { lang, start: 0 });
    }

    if let Some(split) = plan_split(
        full_en, full_he, offsets_en, offsets_he, keys_len, current, en_dict, he_dict,
    ) {
        return Some(split);
    }

    plan_spelling(full_en, full_he, current, en_dict, he_dict, en_freq)
}

/// Second pipeline: the word is not a mistype of the other layout, but it may
/// be a mistype of an English word.
///
/// Only runs when we *know* the layout is English, for two reasons. It is a
/// correctness requirement — the correction is injected as keystrokes, which
/// only produce the intended letters under an English layout — and a safety one:
/// under an unknown or Hebrew layout the English reading of the keys is not what
/// the user is looking at, so "fixing" it would be nonsense.
///
/// The Hebrew reading is also required to be nothing at all, loose match
/// included. A key sequence that reads as a prefixed Hebrew word is the layout
/// pipeline's business (it just wasn't confident enough to switch); rewriting it
/// as an English word would be the two pipelines fighting over one word.
fn plan_spelling(
    full_en: &str,
    full_he: &str,
    current: Option<Language>,
    en_dict: Dict,
    he_dict: Dict,
    en_freq: Freq,
) -> Option<Plan> {
    if current != Some(Language::English) {
        return None;
    }
    if valid_loose(full_he, Language::Hebrew, en_dict, he_dict) {
        return None;
    }
    let text = crate::spell::correct(full_en, en_dict, en_freq)?;
    Some(Plan::Spell { text })
}

/// Missing-space split: opt-in, and only meaningful when we know the layout.
/// Carves `helloעולם` into two words by finding a split where the left side is
/// a real word in the current layout and the right side is a confident word in
/// the other one.
#[allow(clippy::too_many_arguments)]
fn plan_split(
    full_en: &str,
    full_he: &str,
    offsets_en: &[usize],
    offsets_he: &[usize],
    keys_len: usize,
    current: Option<Language>,
    en_dict: Dict,
    he_dict: Dict,
) -> Option<Plan> {
    if !split_enabled() {
        return None;
    }
    let current = current?;
    let other = current.other();

    // The full buffer must be gibberish in *both* layouts before we even
    // consider carving it up; if it reads as a real word either way, it is one
    // word and must be left intact.
    let (full_cur, full_oth) = match current {
        Language::English => (full_en, full_he),
        Language::Hebrew => (full_he, full_en),
    };
    if valid_loose(full_cur, current, en_dict, he_dict)
        || valid_loose(full_oth, other, en_dict, he_dict)
    {
        return None;
    }

    // Scan split points from the longest prefix down — the first match leaves
    // the most user-typed text intact. Require: a real word (current layout) on
    // the left, and a confident word (other layout, ≥2 chars) on the right that
    // is NOT itself a real word in the current layout.
    for split in (1..keys_len).rev() {
        let (cur_prefix, cur_suffix) = match current {
            Language::English => (
                &full_en[..offsets_en[split]],
                &full_en[offsets_en[split]..],
            ),
            Language::Hebrew => (
                &full_he[..offsets_he[split]],
                &full_he[offsets_he[split]..],
            ),
        };
        if !valid_strict(cur_prefix, current, en_dict, he_dict) {
            continue;
        }
        let oth_suffix = match other {
            Language::English => &full_en[offsets_en[split]..],
            Language::Hebrew => &full_he[offsets_he[split]..],
        };
        if oth_suffix.chars().count() < 2 {
            continue;
        }
        if valid_strict(oth_suffix, other, en_dict, he_dict)
            && !valid_loose(cur_suffix, current, en_dict, he_dict)
        {
            return Some(Plan::Switch {
                lang: other,
                start: split,
            });
        }
    }

    None
}

/// Run both correction pipelines over a finished key sequence.
///
/// The layout pipeline anchors on the live keyboard layout: a sequence that
/// already reads as a real word in the current layout is never touched, and we
/// only switch when the *other* layout yields a confident dictionary word. A
/// missing-space split fallback exists but is opt-in (`RECAST_SPLIT=1`) because
/// it cannot be made reliably safe. Only if none of that fires does the English
/// spelling autocorrect get a look at the word.
///
/// Returns the single [`Fix`] to apply, or `None` to leave the word alone. For
/// `Fix::Layout` the layout switch has already happened by the time this
/// returns (and `None` is returned instead if the OS refused it); for
/// `Fix::Spelling` no layout call is made at all.
pub fn check_and_correct<K: Copy>(
    keys: &[K],
    to_en: impl Fn(K) -> Option<char>,
    to_he: impl Fn(K) -> Option<char>,
    en_dict: Dict,
    he_dict: Dict,
) -> Option<Fix> {
    if keys.is_empty() {
        return None;
    }

    // Build the full English/Hebrew folds once and record where each key's
    // char lands in the resulting `String`s, so the split scan can slice into
    // the precomputed buffers instead of re-walking the key vector.
    //
    // `offsets_*[k]` is the byte offset *after* the first k keys have been
    // folded, so `&full_en[..offsets_en[k]]` is the prefix for `keys[..k]`
    // and `&full_en[offsets_en[k]..]` is the suffix for `keys[k..]`. Same
    // for Hebrew. Length is `keys.len() + 1`.
    //
    // The offset tables exist only for the missing-space split, which is off by
    // default — when it is, they are not built at all and a finished word costs
    // two short string allocations instead of four.
    let want_offsets = split_enabled();
    let mut full_en = String::with_capacity(keys.len());
    let mut full_he = String::with_capacity(keys.len() * 2);
    let mut offsets_en = Vec::with_capacity(if want_offsets { keys.len() + 1 } else { 0 });
    let mut offsets_he = Vec::with_capacity(if want_offsets { keys.len() + 1 } else { 0 });
    if want_offsets {
        offsets_en.push(0);
        offsets_he.push(0);
    }
    for &k in keys {
        if let Some(c) = to_en(k) {
            full_en.push(c);
        }
        if let Some(c) = to_he(k) {
            full_he.push(c);
        }
        if want_offsets {
            offsets_en.push(full_en.len());
            offsets_he.push(full_he.len());
        }
    }

    let current = crate::layout::current_layout();
    let Some(plan) = plan(
        &full_en,
        &full_he,
        &offsets_en,
        &offsets_he,
        keys.len(),
        current,
        en_dict,
        he_dict,
        en_freq(),
        he_freq(),
    ) else {
        debug_log(&full_en, &full_he, None, false);
        return None;
    };

    match plan {
        Plan::Switch { lang, start } => {
            let switched = switch_layout_to(lang);
            if debug_enabled() && start > 0 {
                println!("split @ {}", start);
            }
            debug_log(&full_en, &full_he, Some(lang), switched);
            // The corrected word, already spelled in the target language, for
            // platforms that insert text instead of replaying keys. A non-zero
            // `start` only ever comes from the split, which only runs when the
            // offset tables were built.
            let (full, offsets) = match lang {
                Language::English => (&full_en, &offsets_en),
                Language::Hebrew => (&full_he, &offsets_he),
            };
            let text = if start == 0 {
                full.clone()
            } else {
                full[offsets[start]..].to_string()
            };
            // The OS refused (or was already on) the target layout: retyping now
            // would re-enter the same characters, so do nothing.
            switched.then_some(Fix::Layout { start, text })
        }
        Plan::Spell { text } => {
            // No layout call at all — the word stays in English, only its
            // letters change.
            debug_log(&full_en, &full_he, None, false);
            if debug_enabled() {
                println!("spell: {} -> {}", full_en, text);
            }
            Some(Fix::Spelling { text })
        }
    }
}

/// Test-only builders: assemble the same sorted-blob layout `build.rs` writes,
/// from a handful of words, and leak it so it has the `'static` lifetime the
/// real (binary-embedded) lists have.
#[cfg(test)]
impl Dict {
    pub(crate) fn of(words: &[&str]) -> Dict {
        let mut words: Vec<&str> = words.to_vec();
        words.sort_unstable();
        words.dedup();
        Dict::new(Box::leak(words.join("\n").into_boxed_str()))
    }
}

#[cfg(test)]
impl Freq {
    /// No word is ranked: the frequency tie-break is inert.
    pub(crate) const EMPTY: Freq = Freq::new("");

    pub(crate) fn of(entries: &[(&str, u32)]) -> Freq {
        let mut entries: Vec<(&str, u32)> = entries.to_vec();
        entries.sort_unstable();
        let blob: Vec<String> = entries.iter().map(|(w, r)| format!("{w}\t{r}")).collect();
        Freq::new(Box::leak(blob.join("\n").into_boxed_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(words: &[&str]) -> Dict {
        Dict::of(words)
    }

    /// Empty frequency list — with these, the tie-break is inert and every test
    /// below exercises the pure dictionary logic unchanged.
    fn nofreq() -> Freq {
        Freq::EMPTY
    }

    /// Frequency list from `(word, rank)` pairs (rank 0 = most common).
    fn freq(entries: &[(&str, u32)]) -> Freq {
        Freq::of(entries)
    }

    #[test]
    fn binary_search_finds_every_entry_and_nothing_else() {
        // The lookup walks back from an arbitrary probe byte to a line start,
        // so exercise it against a list long enough to need several probes.
        let words = ["a", "aa", "ab", "abc", "b", "hello", "helot", "zebra", "שלום"];
        let d = dict(&words);
        for w in words {
            assert!(d.contains(w), "{w}");
        }
        for w in ["", "aaa", "abcd", "he", "hellp", "zebras", "שלו", "~"] {
            assert!(!d.contains(w), "{w}");
        }
        // Ranks come back with their entry, including the last line (no
        // trailing newline in the blob).
        let f = freq(&[("hello", 500), ("a", 0), ("zebra", 12_345)]);
        assert_eq!(f.rank("a"), Some(0));
        assert_eq!(f.rank("hello"), Some(500));
        assert_eq!(f.rank("zebra"), Some(12_345));
        assert_eq!(f.rank("hell"), None);
        assert_eq!(Freq::EMPTY.rank("hello"), None);
    }

    #[test]
    fn embedded_lists_are_sorted_and_searchable() {
        // Guards the build-script contract: a blob that came out unsorted would
        // make every lookup silently unreliable.
        for blob in [en_dict().blob, he_dict().blob] {
            assert!(blob.lines().is_sorted(), "dictionary blob not sorted");
        }
        for blob in [en_freq().blob, he_freq().blob] {
            assert!(
                blob.lines()
                    .map(|l| l.split('\t').next().unwrap())
                    .is_sorted(),
                "frequency blob not sorted"
            );
        }
        assert!(en_dict().contains("hello"));
        assert!(en_dict().contains("dont")); // the folded variant of "don't"
        assert!(!en_dict().contains("zzzqqq"));
        assert!(he_dict().contains("שלום"));
        assert!(en_freq().rank("the").is_some());
        assert!(he_freq().rank("את").is_some());
    }

    // English word "gv" -> these are illustrative ASCII stand-ins; the real
    // tests below use actual Hebrew text so the prefix logic is exercised.
    #[test]
    fn real_word_in_current_layout_is_left_alone() {
        let en = dict(&["hello"]);
        let he = dict(&["שלום"]);
        // Typing "hello" while in English layout: do nothing.
        assert_eq!(decide_known("hello", "ימךךם", Language::English, en, he, nofreq(), nofreq()), None);
        // Typing "שלום" while in Hebrew layout: do nothing.
        assert_eq!(decide_known("akuo", "שלום", Language::Hebrew, en, he, nofreq(), nofreq()), None);
    }

    #[test]
    fn wrong_layout_switches_to_other() {
        let en = dict(&["hello"]);
        let he = dict(&["שלום"]);
        // In Hebrew layout but the keys spell "hello" in English -> switch EN.
        assert_eq!(
            decide_known("hello", "ימךךם", Language::Hebrew, en, he, nofreq(), nofreq()),
            Some(Language::English)
        );
        // In English layout but the keys spell "שלום" in Hebrew -> switch HE.
        assert_eq!(
            decide_known("akuo", "שלום", Language::English, en, he, nofreq(), nofreq()),
            Some(Language::Hebrew)
        );
    }

    #[test]
    fn prefixed_hebrew_is_not_mangled() {
        // "שלום" is in the dict; "ושלום" not, but matches via one‑letter prefix.
        let en = dict(&["hello"]);
        let he = dict(&["שלום"]);
        assert_eq!(decide_known("uakuo", "ושלום", Language::Hebrew, en, he, nofreq(), nofreq()), None);
    }

    #[test]
    fn fun_word_mistyped_in_hebrew_switches() {
        let en = dict(&["fun"]);
        let he = dict(&[]);
        // Hebrew reading = "כום"
        assert_eq!(decide_known("fun", "כום", Language::Hebrew, en, he, nofreq(), nofreq()), Some(Language::English));
    }

    #[test]
    fn very_word_mistyped_in_hebrew_switches() {
        let en = dict(&["very"]);
        let he = dict(&[]);
        // Hebrew reading = "הקרט"
        assert_eq!(decide_known("very", "הקרט", Language::Hebrew, en, he, nofreq(), nofreq()), Some(Language::English));
    }

    #[test]
    fn prefixed_hebrew_with_english_collision_always_switches() {
        // "ושלום" prefixed Hebrew word collides with English "uakuo".
        // New logic switches to other layout when other dict has word.
        let en = dict(&["uakuo"]);
        let he = dict(&["שלום"]);
        assert_eq!(decide_known("uakuo", "ושלום", Language::Hebrew, en, he, nofreq(), nofreq()), Some(Language::English));
    }


    #[test]
    fn short_gibberish_never_switches() {
        // Short sequence that is a word in NEITHER dict must not trigger a
        // switch, regardless of the short-word config.
        let en = dict(&[]);
        let he = dict(&[]);
        assert_eq!(decide_known("xkc", "סלב", Language::English, en, he, nofreq(), nofreq()), None);
        assert_eq!(decide_unknown("xkc", "סלב", en, he, nofreq(), nofreq()), None);
    }

    #[test]
    fn unknown_layout_switches_only_on_one_sided_match() {
        let en = dict(&["hello"]);
        let he = dict(&["שלום"]);
        assert_eq!(
            decide_unknown("hello", "ימךךם", en, he, nofreq(), nofreq()),
            Some(Language::English)
        );
        assert_eq!(
            decide_unknown("akuo", "שלום", en, he, nofreq(), nofreq()),
            Some(Language::Hebrew)
        );
        // In neither dict → no switch.
        assert_eq!(decide_unknown("qqqq", "ננננ", en, he, nofreq(), nofreq()), None);
    }

    #[test]
    fn ambiguous_homograph_never_switches() {
        // Keys valid as a word in BOTH layouts: do not switch, preserve user intent.
        let en = dict(&["go"]);
        let he = dict(&["עט"]); // whatever the keys read as in Hebrew
        assert_eq!(decide_known("go", "עט", Language::English, en, he, nofreq(), nofreq()), None);
        assert_eq!(decide_known("go", "עט", Language::Hebrew, en, he, nofreq(), nofreq()), None);
    }

    #[test]
    fn homograph_switches_to_far_more_common_reading() {
        // Both readings are real words. In Hebrew layout, but the current
        // reading ("עט") is rare/unranked while the English reading ("go") is a
        // top-2000 word → the frequency tie-break switches to English.
        let en = dict(&["go"]);
        let he = dict(&["עט"]);
        let en_f = freq(&[("go", 30)]); // very common
        let he_f = nofreq(); // "עט" absent from the top-N list
        assert_eq!(
            decide_known("go", "עט", Language::Hebrew, en, he, en_f, he_f),
            Some(Language::English)
        );
    }

    #[test]
    fn homograph_keeps_current_when_both_common() {
        // Both readings are common words → no decisive winner, keep current.
        let en = dict(&["go"]);
        let he = dict(&["עט"]);
        let en_f = freq(&[("go", 30)]);
        let he_f = freq(&[("עט", 40)]); // comparably common, within the factor
        assert_eq!(
            decide_known("go", "עט", Language::Hebrew, en, he, en_f, he_f),
            None
        );
    }

    /// Run the planner on an already-folded pair of readings. The offset tables
    /// and key count only matter to the missing-space split, which is off by
    /// default, so tests that don't exercise it can pass empty ones.
    fn plan_for(
        en_text: &str,
        he_text: &str,
        current: Option<Language>,
        en: Dict,
        he: Dict,
        en_f: Freq,
    ) -> Option<Plan> {
        plan(en_text, he_text, &[], &[], 0, current, en, he, en_f, nofreq())
    }

    #[test]
    fn spelling_fixes_a_typo_the_layout_pipeline_passed_on() {
        let en = dict(&["hello"]);
        let he = dict(&["שלום"]);
        let en_f = freq(&[("hello", 500)]);
        // "helo" is not a word in English and its Hebrew reading is gibberish,
        // so the layout pipeline declines — and the speller takes over.
        assert_eq!(
            plan_for("helo", "יקךם", Some(Language::English), en, he, en_f),
            Some(Plan::Spell { text: "hello".to_string() })
        );
    }

    #[test]
    fn layout_switch_wins_over_spelling() {
        // The keys are one edit from "hello" AND spell a real Hebrew word. The
        // layout reading is exact rather than a guess, so it wins — and because
        // a single plan comes back, the speller never also runs.
        let en = dict(&["hello"]);
        let he = dict(&["שלום"]);
        let en_f = freq(&[("hello", 500)]);
        assert_eq!(
            plan_for("helo", "שלום", Some(Language::English), en, he, en_f),
            Some(Plan::Switch { lang: Language::Hebrew, start: 0 })
        );
    }

    #[test]
    fn spell_corrected_word_is_not_also_layout_switched() {
        // The user's case: a slight misspelling gets corrected, and the fact
        // that the *corrected* word's keys also read as a real Hebrew word must
        // not flip it. The plan is a Spell, which performs no layout switch,
        // and the injected keys are never fed back through the checker.
        let en = dict(&["hello"]);
        // "ימךךם" is what the keys of "hello" read as in Hebrew — a dictionary
        // word here, so a re-check would have switched.
        let he = dict(&["ימךךם", "שלום"]);
        let en_f = freq(&[("hello", 500)]);
        let plan = plan_for("helo", "יקךם", Some(Language::English), en, he, en_f);
        assert_eq!(plan, Some(Plan::Spell { text: "hello".to_string() }));
        assert!(!matches!(plan, Some(Plan::Switch { .. })));
    }

    #[test]
    fn spelling_yields_to_a_hebrew_reading() {
        // The Hebrew reading is a prefixed real word: not confident enough for
        // the layout pipeline to switch, but definitely not ours to rewrite.
        let en = dict(&["hello"]);
        let he = dict(&["שלום"]);
        let en_f = freq(&[("hello", 500)]);
        assert_eq!(
            plan_for("helo", "ושלום", Some(Language::English), en, he, en_f),
            None
        );
    }

    #[test]
    fn spelling_only_runs_in_a_known_english_layout() {
        // Corrections are injected as keystrokes, so they only produce the
        // intended letters under an English layout — anywhere else, hands off.
        let en = dict(&["hello"]);
        let he = dict(&[]);
        let en_f = freq(&[("hello", 500)]);
        assert_eq!(plan_for("helo", "יקךם", Some(Language::Hebrew), en, he, en_f), None);
        assert_eq!(plan_for("helo", "יקךם", None, en, he, en_f), None);
    }

    #[test]
    fn known_word_is_never_spell_corrected() {
        // A real English word is left alone even when a far more common word is
        // one edit away.
        let en = dict(&["form", "from"]);
        let he = dict(&[]);
        let en_f = freq(&[("from", 10), ("form", 900)]);
        assert_eq!(plan_for("form", "בםרצ", Some(Language::English), en, he, en_f), None);
    }

    #[test]
    fn homograph_no_switch_when_other_reading_uncommon() {
        // The other reading is a real word but not common enough (rank beyond
        // FREQ_COMMON_MAX) → don't override the current layout.
        let en = dict(&["go"]);
        let he = dict(&["עט"]);
        let en_f = freq(&[("go", 9000)]); // real word, but rare
        let he_f = nofreq();
        assert_eq!(
            decide_known("go", "עט", Language::Hebrew, en, he, en_f, he_f),
            None
        );
    }
}
