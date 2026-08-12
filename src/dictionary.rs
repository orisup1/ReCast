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

    /// Call `f(word, rank)` for every entry starting with `prefix`, in list
    /// order.
    ///
    /// Because the blob is sorted, those entries are one contiguous run: a
    /// binary search finds where it starts and the walk stops at the first line
    /// that no longer matches. This is what lets the speller and the completer
    /// consider "every common word beginning with h" without paying for the
    /// other 96% of the list.
    pub fn for_each_with_prefix(self, prefix: &str, mut f: impl FnMut(&str, u32)) {
        let blob = self.blob.as_bytes();
        let mut pos = lower_bound(blob, prefix.as_bytes());
        while pos < blob.len() {
            let end = pos + blob[pos..]
                .iter()
                .position(|&b| b == b'\n')
                .unwrap_or(blob.len() - pos);
            let line = &blob[pos..end];
            let key = key_of(line);
            if !key.starts_with(prefix.as_bytes()) {
                return;
            }
            if let (Ok(word), Some(rank)) = (
                std::str::from_utf8(key),
                line.get(key.len() + 1..).and_then(parse_rank),
            ) {
                f(word, rank);
            }
            pos = end + 1;
        }
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

/// Byte offset of the first line whose key is `>= needle`, or the end of the
/// blob when there is none. The mirror of [`lookup`] for range queries: the
/// same walk-back-to-a-line-start probe, keeping the answer on a line boundary
/// so the caller can read forward from it.
fn lower_bound(blob: &[u8], needle: &[u8]) -> usize {
    let (mut lo, mut hi) = (0usize, blob.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let start = match blob[lo..mid].iter().rposition(|&b| b == b'\n') {
            Some(i) => lo + i + 1,
            None => lo,
        };
        let end = start
            + blob[start..]
                .iter()
                .position(|&b| b == b'\n')
                .unwrap_or(blob.len() - start);
        if key_of(&blob[start..end]) < needle {
            lo = end + 1;
        } else {
            hi = start;
        }
    }
    lo.min(blob.len())
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

/// Whether to log every word check and switch decision.
///
/// `RECAST_DEBUG=0` used to mean *on* — the flag was presence-only, as an
/// environment variable can afford to be. It goes through the same reader as
/// every other switch now, because `debug = false` sitting in a config file and
/// turning logging on would be indefensible.
fn debug_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| crate::config::flag("RECAST_DEBUG", false))
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

/// The capitalization the user typed, recovered from the shift/caps-lock state
/// of each key. The word buffers hold key *positions*, which say nothing about
/// case on their own, so this is tracked alongside them and re-applied to
/// whatever the pipelines decide to type back — otherwise correcting `Helo`
/// would quietly hand back a lowercase `hello`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    /// No shift anywhere, or a mix too irregular to reproduce.
    Lower,
    /// First letter shifted, the rest not: a sentence opener or a name.
    Title,
    /// Every letter shifted: an acronym, or someone shouting.
    Upper,
}

impl Case {
    /// The case pattern of a word whose letters were typed with these shift
    /// states.
    fn of(shifted: &[bool]) -> Case {
        match shifted.split_first() {
            None => Case::Lower,
            Some((false, _)) => Case::Lower,
            // A single shifted letter reads as a capital, not as an acronym.
            Some((true, [])) => Case::Title,
            Some((true, rest)) if rest.iter().all(|&s| s) => Case::Upper,
            Some((true, rest)) if rest.iter().all(|&s| !s) => Case::Title,
            // Anything else (sHiFtY) is not a pattern worth reproducing.
            Some((true, _)) => Case::Lower,
        }
    }

    /// Re-apply the pattern to a replacement word. Only ASCII letters are
    /// touched, so a Hebrew reading (which has no case) passes through
    /// unchanged, as does an expansion's punctuation.
    fn apply(self, text: &str) -> String {
        match self {
            Case::Lower => text.to_string(),
            Case::Upper => text.to_uppercase(),
            Case::Title => {
                let mut out = String::with_capacity(text.len());
                let mut chars = text.chars();
                if let Some(first) = chars.next() {
                    out.extend(first.to_uppercase());
                    out.push_str(chars.as_str());
                }
                out
            }
        }
    }
}

/// What the correction pipelines decided to do with a finished word.
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
    /// in the *new* layout, capitalization included) for platforms that insert
    /// text directly, and replaying `keys[start..]` produces exactly the same
    /// characters now that the layout has changed, for platforms that can only
    /// send keycodes. `lang` is the layout that was switched to — a keycode
    /// replay needs it to know whether re-pressing shift reproduces the user's
    /// capitals (English) or mangles the word (Hebrew has no case, and shift
    /// there types punctuation).
    Layout {
        start: usize,
        text: String,
        lang: Language,
    },
    /// Rewrite in the current (English) layout, from the speller or from an
    /// abbreviation expansion: the layout is untouched and the caller should
    /// erase the whole word and type `text` instead. `text` is ASCII and
    /// typeable through the English keymap, but — unlike before case tracking —
    /// it may contain capitals, and an expansion may contain spaces.
    Spelling { text: String },
}

/// One key sequence read under one layout, split at the word's end.
///
/// A finished word is rarely just letters: people end clauses with `,` and
/// sentences with `.`, and those characters are in the buffer like any other.
/// Asking the dictionary about `hello,` gets a miss, which is why a mistyped
/// word used to go uncorrected precisely where words most often end — and why a
/// spelling fix, retyped from the word alone, used to swallow the comma with
/// it. So the trailing punctuation is set aside once, here: [`word`](Self::word)
/// is what the dictionaries are asked about, [`tail`](Self::tail) is what has to
/// be put back after whatever they answer, and `full` is the two together — the
/// characters actually on screen, one per key.
///
/// Only the *trailing* run is separated. Punctuation inside a word is part of it
/// (`don't`), and a leading `(` is left in place because both readings have to
/// keep agreeing character-for-character with the keys, which is what lets a
/// correction be erased and put back by count.
#[derive(Clone, Copy)]
struct Reading<'a> {
    /// Everything the keys spell in this layout.
    full: &'a str,
    /// The word: `full` up to the trailing punctuation.
    word: &'a str,
    /// The trailing punctuation, `""` when the word ends the buffer.
    tail: &'a str,
}

/// Whether a character the keys produced belongs to the word itself, rather
/// than to the punctuation trailing it.
///
/// `trusted_shift` says whether the map that produced `c` knows about shift.
/// The English map does, so `!` arrives as `!` and can be set aside. The Hebrew
/// map does not — every key gives its unshifted character — so a character
/// typed with shift held is *not* known to be what is on screen, and stripping
/// it would mean putting something else back in its place. There it counts as
/// part of the word, which at worst leaves the word unrecognised: the same
/// outcome as before any of this existed.
fn in_word(c: char, shift: bool, trusted_shift: bool) -> bool {
    c.is_alphanumeric() || (shift && !trusted_shift)
}

impl<'a> Reading<'a> {
    /// `word_end` is the byte offset the caller recorded while folding: the end
    /// of the last character that counts as part of the word.
    fn new(full: &'a str, word_end: usize) -> Self {
        Self {
            full,
            word: &full[..word_end],
            tail: &full[word_end..],
        }
    }

    #[cfg(test)]
    fn of(full: &'a str) -> Self {
        Self::new(full, full.len())
    }
}

/// Internal counterpart of [`Fix`] that still carries the target language, so
/// the pure planner can be tested without performing the switch.
#[derive(Clone, Debug, PartialEq)]
enum Plan {
    Switch { lang: Language, start: usize },
    Spell { text: String },
    /// A user-configured abbreviation was typed out in full.
    Expand { text: String },
}

/// What a spelling fix or an expansion puts on screen: the new word in the case
/// the old one was typed in, followed by the punctuation that ended it.
///
/// The caller erases every character the user typed, the punctuation included —
/// it sits between the cursor and the word, so there is no erasing around it —
/// which means anything left out here is deleted from the user's text. That is
/// what used to happen to the comma in `recieve,`.
fn respelled(fixed: &str, case: Case, tail: &str) -> String {
    let mut out = case.apply(fixed);
    out.push_str(tail);
    out
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
/// The pipelines run in a fixed order and the first one to produce a plan wins
/// — a word is only ever corrected once:
///
///   0. **Abbreviation expansion** (English only). The user wrote this rule
///      down themselves, so nothing gets to overrule it.
///   1. **Layout** (whole buffer, then the opt-in missing-space split). It goes
///      ahead of the speller because it is the *exact* signal: the keystrokes
///      literally spell a real word in the other language, no guessing involved.
///   2. **Spelling** (English only). Reached only when the layout pipeline
///      declined, i.e. the keystrokes are not a word in either language. The
///      resulting word is typed as-is and never re-examined by the layout
///      pipeline, so a spell-corrected word whose keys happen to also read as a
///      Hebrew word is *not* subsequently flipped.
#[allow(clippy::too_many_arguments)]
fn plan(
    en: Reading,
    he: Reading,
    offsets_en: &[usize],
    offsets_he: &[usize],
    keys_len: usize,
    current: Option<Language>,
    case: Case,
    en_dict: Dict,
    he_dict: Dict,
    en_freq: Freq,
    he_freq: Freq,
) -> Option<Plan> {
    // Every pipeline below asks about the *word* — the punctuation the user
    // finished it with is set aside by `Reading` and put back by the caller.
    // The split scan is the one exception: its offsets index the full readings.
    let (word_en, word_he) = (en.word, he.word);
    // A word the user has already taken back with the undo gesture is not
    // offered a second opinion. This is checked before everything else,
    // expansions included: the reading being suppressed is the one the user
    // saw, chose to keep, and would otherwise have to fight for again on every
    // repetition (see `complete::suppressed`).
    let typed = match current {
        Some(Language::Hebrew) => word_he,
        _ => word_en,
    };
    if crate::complete::suppressed(typed) {
        return None;
    }

    // An expansion the user configured by hand outranks everything we infer.
    if current == Some(Language::English) {
        if let Some(text) = crate::complete::expand(word_en) {
            return Some(Plan::Expand { text });
        }
    }

    // Whole-buffer decision first — this is what fires for virtually every real
    // correction.
    let whole = match current {
        Some(cur) => decide_known(word_en, word_he, cur, en_dict, he_dict, en_freq, he_freq),
        None => decide_unknown(word_en, word_he, en_dict, he_dict, en_freq, he_freq),
    };
    if let Some(lang) = whole {
        return Some(Plan::Switch { lang, start: 0 });
    }

    if let Some(split) = plan_split(
        en.full, he.full, offsets_en, offsets_he, keys_len, current, en_dict, he_dict,
    ) {
        return Some(split);
    }

    plan_spelling(word_en, word_he, current, case, en_dict, he_dict, en_freq)
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
/// An all-caps token is an acronym (`NASA`, `HTTP`, a ticker, an env var), not
/// a misspelling — the dictionary has no opinion on it and the speller would
/// happily turn it into the nearest common word. Case tracking is what makes
/// this distinction visible at all.
fn plan_spelling(
    full_en: &str,
    full_he: &str,
    current: Option<Language>,
    case: Case,
    en_dict: Dict,
    he_dict: Dict,
    en_freq: Freq,
) -> Option<Plan> {
    if current != Some(Language::English) {
        return None;
    }
    if case == Case::Upper {
        return None;
    }
    // A word the user has declared theirs is never second-guessed.
    if crate::complete::ignored(full_en) {
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
    shift_of: impl Fn(K) -> bool,
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
    // Shift state of the keys that produced an English *letter*, which is what
    // the case pattern is read off. Digits and punctuation have no case to
    // contribute, and the shift behind a `!` would otherwise read as a capital
    // and break the pattern.
    let mut shifted_en = Vec::with_capacity(keys.len());
    if want_offsets {
        offsets_en.push(0);
        offsets_he.push(0);
    }
    // Where the word ends in each reading: everything after it is the
    // punctuation the user finished with, which the dictionaries must not see
    // and the correction must not eat (see `Reading`).
    let mut word_end_en = 0usize;
    let mut word_end_he = 0usize;
    for &k in keys {
        let shift = shift_of(k);
        if let Some(c) = to_en(k) {
            full_en.push(c);
            if in_word(c, shift, true) {
                word_end_en = full_en.len();
            }
            // Case is read off letters alone: the shift behind a `!` says
            // nothing about whether the word was capitalized.
            if c.is_ascii_alphabetic() {
                shifted_en.push(shift);
            }
        }
        if let Some(c) = to_he(k) {
            full_he.push(c);
            if in_word(c, shift, false) {
                word_end_he = full_he.len();
            }
        }
        if want_offsets {
            offsets_en.push(full_en.len());
            offsets_he.push(full_he.len());
        }
    }
    let case = Case::of(&shifted_en);
    let en = Reading::new(&full_en, word_end_en);
    let he = Reading::new(&full_he, word_end_he);

    let current = crate::layout::current_layout();
    let Some(plan) = plan(
        en,
        he,
        &offsets_en,
        &offsets_he,
        keys.len(),
        current,
        case,
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
            let switched = switch_layout_to(lang).changed();
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
            // Only an English target has capitals to restore; Hebrew has no
            // case at all, so the pattern is meaningless there.
            let text = if lang == Language::English && start == 0 {
                case.apply(&text)
            } else {
                text
            };
            // The OS refused (or was already on) the target layout: retyping now
            // would re-enter the same characters, so do nothing.
            switched.then_some(Fix::Layout { start, text, lang })
        }
        Plan::Spell { text } | Plan::Expand { text } => {
            // No layout call at all — the word stays in English, only its
            // letters change.
            debug_log(&full_en, &full_he, None, false);
            if debug_enabled() {
                println!("spell: {} -> {}{}", full_en, text, en.tail);
            }
            Some(Fix::Spelling {
                text: respelled(&text, case, en.tail),
            })
        }
    }
}

/// The word `keys` spells, if the pipelines left it alone *only* because the
/// user has it on one of their lists — `ignore.txt` or the session list a
/// previous undo put it on.
///
/// Called by the platform listeners when [`check_and_correct`] declined, to
/// decide whether the Ctrl double-tap has anything to offer. The gesture is a
/// toggle: it takes back a correction that happened, and takes a word off the
/// list when a correction *didn't* happen for that reason. Nothing else arms
/// it, so a word that is simply spelled correctly is untouched by it.
///
/// The two lists are checked against different readings, and deliberately so.
/// A session entry came from undoing what the user was looking at, so it
/// suppresses the reading under the live layout. `ignore.txt` is the speller's
/// escape hatch and only ever gated the English reading — a word on it that is
/// typed in the wrong layout should still be layout-corrected, so it is not
/// consulted outside English.
pub fn declined_by_list<K: Copy>(
    keys: &[K],
    to_en: impl Fn(K) -> Option<char>,
    to_he: impl Fn(K) -> Option<char>,
    shift_of: impl Fn(K) -> bool,
) -> Option<String> {
    if keys.is_empty() {
        return None;
    }
    let mut full_en = String::with_capacity(keys.len());
    let mut full_he = String::with_capacity(keys.len() * 2);
    let mut word_end_en = 0usize;
    let mut word_end_he = 0usize;
    for &k in keys {
        let shift = shift_of(k);
        if let Some(c) = to_en(k) {
            full_en.push(c);
            if in_word(c, shift, true) {
                word_end_en = full_en.len();
            }
        }
        if let Some(c) = to_he(k) {
            full_he.push(c);
            if in_word(c, shift, false) {
                word_end_he = full_he.len();
            }
        }
    }
    // The lists hold words, so they are asked about the word — the same reading
    // `plan` would have checked them against, punctuation set aside.
    let current = crate::layout::current_layout();
    let typed = match current {
        Some(Language::Hebrew) => &full_he[..word_end_he],
        _ => &full_en[..word_end_en],
    };
    if crate::complete::suppressed(typed) {
        return Some(typed.to_string());
    }
    let word_en = &full_en[..word_end_en];
    if current == Some(Language::English) && crate::complete::ignored(word_en) {
        return Some(word_en.to_string());
    }
    None
}

/// Complete the partial word the user has typed so far, on an explicit request
/// (the completion key — see the platform listeners).
///
/// Unlike the correction pipelines this fires *mid-word*, with no terminator
/// typed and nothing wrong with the input: the user asked. It still refuses
/// under a non-English layout for the same reason the speller does — the result
/// is injected as English text or English keystrokes, and under Hebrew that is
/// not what the user is looking at.
///
/// Returns the candidates in offer order (each already capitalized to match
/// what was typed), for the caller to swap in one at a time as the completion
/// key is tapped again. Empty means there is nothing worth offering.
pub fn complete_candidates<K: Copy>(
    keys: &[K],
    to_en: impl Fn(K) -> Option<char>,
    shift_of: impl Fn(K) -> bool,
    en_dict: Dict,
) -> Vec<String> {
    if keys.is_empty() || crate::layout::current_layout() != Some(Language::English) {
        return Vec::new();
    }
    let mut prefix = String::with_capacity(keys.len());
    let mut shifted = Vec::with_capacity(keys.len());
    for &k in keys {
        if let Some(c) = to_en(k) {
            prefix.push(c);
            shifted.push(shift_of(k));
        }
    }
    let words = crate::complete::completions(&prefix, en_dict, en_freq());
    if debug_enabled() && !words.is_empty() {
        println!("complete: {} -> {}", prefix, words.join(" | "));
    }
    let case = Case::of(&shifted);
    words.into_iter().map(|w| case.apply(&w)).collect()
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
    fn prefix_scan_yields_exactly_the_matching_run() {
        // The range query the speller and the completer are both built on: it
        // has to start at the first match (not the first line that merely sorts
        // after the prefix) and stop at the first non-match.
        let f = freq(&[
            ("a", 0),
            ("hell", 4),
            ("hello", 1),
            ("help", 2),
            ("helot", 3),
            ("zebra", 5),
        ]);
        let collect = |prefix: &str| {
            let mut out: Vec<(String, u32)> = Vec::new();
            f.for_each_with_prefix(prefix, |w, r| out.push((w.to_string(), r)));
            out
        };
        assert_eq!(
            collect("hel"),
            vec![
                ("hell".to_string(), 4),
                ("hello".to_string(), 1),
                ("helot".to_string(), 3),
                ("help".to_string(), 2),
            ]
        );
        assert_eq!(collect("zebra"), vec![("zebra".to_string(), 5)]);
        assert!(collect("q").is_empty(), "no match anywhere in the middle");
        assert!(collect("zz").is_empty(), "no match past the end");
        assert_eq!(collect("").len(), 6, "an empty prefix matches everything");
        // The same query over the real 50k-entry blob.
        let mut seen = 0usize;
        en_freq().for_each_with_prefix("keyboa", |w, _| {
            assert!(w.starts_with("keyboa"), "{w}");
            seen += 1;
        });
        assert!(seen > 0, "the real list has words starting with 'keyboa'");
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
        plan_cased(en_text, he_text, current, Case::Lower, en, he, en_f)
    }

    fn plan_cased(
        en_text: &str,
        he_text: &str,
        current: Option<Language>,
        case: Case,
        en: Dict,
        he: Dict,
        en_f: Freq,
    ) -> Option<Plan> {
        plan(
            Reading::of(en_text),
            Reading::of(he_text),
            &[],
            &[],
            0,
            current,
            case,
            en,
            he,
            en_f,
            nofreq(),
        )
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
    fn case_is_read_off_the_shift_states() {
        assert_eq!(Case::of(&[false, false, false]), Case::Lower);
        assert_eq!(Case::of(&[true, false, false]), Case::Title);
        assert_eq!(Case::of(&[true, true, true]), Case::Upper);
        assert_eq!(Case::of(&[true]), Case::Title, "one letter is a capital");
        assert_eq!(Case::of(&[true, false, true]), Case::Lower, "no pattern");
        assert_eq!(Case::of(&[]), Case::Lower);
    }

    #[test]
    fn case_is_reapplied_to_the_replacement() {
        assert_eq!(Case::Lower.apply("hello"), "hello");
        assert_eq!(Case::Title.apply("hello"), "Hello");
        assert_eq!(Case::Upper.apply("hello"), "HELLO");
        // An expansion keeps its own inner capitals under Title case.
        assert_eq!(Case::Title.apply("by the way"), "By the way");
        // Hebrew has no case, so the pattern is a no-op there.
        assert_eq!(Case::Title.apply("שלום"), "שלום");
    }

    #[test]
    fn an_all_caps_token_is_never_spell_corrected() {
        // Acronyms are not misspellings: "NASA" is not a mistyped "nada".
        let en = dict(&["nada"]);
        let he = dict(&[]);
        let en_f = freq(&[("nada", 500)]);
        assert_eq!(
            plan_cased("nasa", "מקק", Some(Language::English), Case::Upper, en, he, en_f),
            None
        );
        // The same letters typed normally are still fair game.
        assert_eq!(
            plan_cased("nasa", "מקק", Some(Language::English), Case::Lower, en, he, en_f),
            Some(Plan::Spell { text: "nada".to_string() })
        );
    }

    #[test]
    fn an_all_caps_word_is_still_layout_switched() {
        // Case only silences the *speller*: keys that literally spell a Hebrew
        // word were still typed in the wrong layout, shouting or not.
        let en = dict(&[]);
        let he = dict(&["שלום"]);
        assert_eq!(
            plan_cased("akuo", "שלום", Some(Language::English), Case::Upper, en, he, nofreq()),
            Some(Plan::Switch { lang: Language::Hebrew, start: 0 })
        );
    }

    /// Build the reading the fold would produce for `text`, where `shifted`
    /// lists the characters that were typed with shift held.
    fn read<'a>(text: &'a str, trusted_shift: bool, shifted: &[char]) -> Reading<'a> {
        let mut word_end = 0;
        for (i, c) in text.char_indices() {
            if in_word(c, shifted.contains(&c), trusted_shift) {
                word_end = i + c.len_utf8();
            }
        }
        Reading::new(text, word_end)
    }

    #[test]
    fn a_word_ends_before_the_punctuation_that_follows_it() {
        // The end of a clause or a sentence is where words most often end, so
        // this is the difference between correcting most of what is typed and
        // correcting only what is followed by a space.
        let r = read("hello,", true, &[]);
        assert_eq!(r.word, "hello");
        assert_eq!(r.tail, ",");
        assert_eq!(r.full, "hello,");
        // Several at once — a quoted word ending a sentence.
        assert_eq!(read("hello.\"", true, &['"']).word, "hello");
        // Punctuation inside a word is part of it.
        assert_eq!(read("don't", true, &[]).word, "don't");
        // A digit is not punctuation: `utf8` is one token, not `utf` plus junk.
        assert_eq!(read("utf8", true, &[]).word, "utf8");
        // Nothing to set aside.
        assert_eq!(read("hello", true, &[]).tail, "");
        // The whole buffer is punctuation: no word at all, and no panic.
        assert_eq!(read("...", true, &[]).word, "");
        // Under a map with no shifted forms (Hebrew), a character typed with
        // shift is kept: we don't know it is really the character on screen.
        assert_eq!(read("שלום1", false, &['1']).word, "שלום1");
        assert_eq!(read("שלום.", false, &[]).word, "שלום");
    }

    #[test]
    fn a_word_typed_in_the_wrong_layout_is_still_fixed_at_a_full_stop() {
        // The keys spell "שלום" plus the Hebrew layout's period. Before the
        // word/punctuation split this asked the dictionary about "שלום." and
        // got nothing, so ending a sentence meant losing the correction.
        let en = dict(&["hello"]);
        let he = dict(&["שלום"]);
        let plan = plan(
            read("akuo/", true, &[]),
            read("שלום.", false, &[]),
            &[],
            &[],
            0,
            Some(Language::English),
            Case::Lower,
            en,
            he,
            nofreq(),
            nofreq(),
        );
        assert_eq!(plan, Some(Plan::Switch { lang: Language::Hebrew, start: 0 }));
    }

    #[test]
    fn a_spelling_fix_keeps_the_punctuation_it_was_typed_with() {
        // The caller erases the comma along with the word, so a replacement
        // without it deletes it from the user's text.
        assert_eq!(respelled("receive", Case::Lower, ","), "receive,");
        assert_eq!(respelled("receive", Case::Title, "."), "Receive.");
        assert_eq!(respelled("receive", Case::Lower, ""), "receive");
        // The case pattern belongs to the word, not to what follows it.
        assert_eq!(respelled("by the way", Case::Title, "!"), "By the way!");
    }

    #[test]
    fn the_speller_sees_the_word_without_its_punctuation() {
        let en = dict(&["hello"]);
        let he = dict(&[]);
        let en_f = freq(&[("hello", 500)]);
        assert_eq!(
            plan(
                read("helo,", true, &[]),
                read("יקךם,", false, &[]),
                &[],
                &[],
                0,
                Some(Language::English),
                Case::Lower,
                en,
                he,
                en_f,
                nofreq(),
            ),
            Some(Plan::Spell { text: "hello".to_string() })
        );
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
