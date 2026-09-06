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

use std::collections::VecDeque;
use std::sync::OnceLock;

mod blob;
pub use blob::{en_dict, en_freq, he_dict, he_freq, Dict, Freq};

use crate::config::Config;
use crate::types::Language;

/// Whether to log every word check and switch decision.
///
/// `RECAST_DEBUG=0` used to mean *on* — the flag was presence-only, as an
/// environment variable can afford to be. It goes through the same reader as
/// every other switch now, because `debug = false` sitting in a config file and
/// turning logging on would be indefensible.
fn debug_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| crate::settings::flag("RECAST_DEBUG", false))
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

// ─────────────────────────────────────────────────────────────────────────────
// The language run — what the words before this one were written in.
//
// Every decision below used to be made about one word in isolation, which threw
// away the strongest signal there is: language comes in runs. Nobody writes one
// Hebrew word between two English ones. A key sequence that reads as a real word
// in both layouts is genuinely ambiguous on its own and not ambiguous at all
// after three Hebrew words, and that is precisely the case the tie-break below
// had to be tuned painfully tight for.
//
// The history itself lives in the listener (`platform::engine::AppState`), which
// is the only thing that knows when a word is finished. What reaches the pure
// decision code is this summary, so the decision stays a function of its
// arguments and stays testable.
// ─────────────────────────────────────────────────────────────────────────────

/// How many finished words back the run is remembered. Long enough to establish
/// a language, short enough that a paragraph in the other one takes over
/// quickly.
const RUN_MEMORY: usize = 8;

/// The unbroken run of one language immediately before the word being decided.
///
/// `len` counts only the words at the end of the history that agree, so a run is
/// broken by the first word of the other language rather than diluted by it —
/// "the last eight words were mostly Hebrew" is a much weaker claim than "the
/// last three words were Hebrew", and it is the second one that should move a
/// decision.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Run {
    /// The language of the run, or `None` when there is no history at all.
    pub lang: Option<Language>,
    /// How many words long it is.
    pub len: u8,
}

impl Run {
    /// Whether the run is long enough, and in the right language, to count as
    /// evidence for `lang`.
    fn favours(self, lang: Language) -> bool {
        self.lang == Some(lang) && self.len >= RUN_MIN
    }
}

/// Shortest run that is allowed to influence anything.
///
/// Two rather than one: a single word is the weakest possible evidence, and it
/// is also the word most likely to have been mis-attributed — the run is built
/// out of the pipelines' own conclusions, so letting one of them feed straight
/// back in would let a single wrong call justify the next one.
const RUN_MIN: u8 = 2;

/// The languages of the last few finished words, newest last.
///
/// Only words whose language is actually *knowable* are recorded (see
/// [`observed`]): an unknown token — a name, a handle, a typo nothing matched —
/// is not evidence about the language being written, and pushing a guess for it
/// would turn the run into noise.
#[derive(Default)]
#[allow(dead_code)]
pub struct History {
    recent: VecDeque<Language>,
}

#[allow(dead_code)]
impl History {
    /// Note the language a finished word turned out to be.
    pub fn push(&mut self, lang: Language) {
        self.recent.push_back(lang);
        while self.recent.len() > RUN_MEMORY {
            self.recent.pop_front();
        }
    }

    /// Forget everything. Called when the cursor moves somewhere else — a
    /// click, an arrow key, a new window — because the run is a claim about the
    /// text being written *here*, and it stops being true the moment that is a
    /// different piece of text.
    pub fn clear(&mut self) {
        self.recent.clear();
    }

    /// The run at the end of the history.
    pub fn run(&self) -> Run {
        let mut iter = self.recent.iter().rev();
        let Some(&lang) = iter.next() else {
            return Run::default();
        };
        let len = 1 + iter.take_while(|&&l| l == lang).count();
        Run {
            lang: Some(lang),
            // Saturating rather than wrapping: `RUN_MEMORY` keeps this far
            // inside a `u8` today, and a cast that silently becomes 0 if that
            // ever changes is not worth the byte it saves.
            len: len.min(u8::MAX as usize) as u8,
        }
    }
}

// Homograph tie-break tuning. When a key sequence is a real word in *both*
// layouts we normally keep the current layout; we only override that when the
// OTHER reading is decisively the one the user more likely meant.
//
// Two sets of thresholds, because the question is a different one depending on
// what came before. With no run to go on, the only thing arguing for the other
// layout is the word itself, and the bar is high enough that it almost never
// fires. With the run already in the other language, the same evidence arrives
// on top of a standing reason to believe the user is writing that language, and
// holding out for a top-2000 word ten times commoner than the alternative would
// be ignoring most of what we know.
const FREQ_COMMON_MAX: u32 = 2000; // the "other" reading must rank at least this common
const FREQ_RARER_FACTOR: u32 = 10; // and be >= this many times more common than current

/// The same two, for a word arriving at the end of a run in the other language.
const FREQ_COMMON_MAX_RUN: u32 = 20_000;
const FREQ_RARER_FACTOR_RUN: u32 = 2;

/// Homograph tie-break: both `cur_text` (current layout) and `oth_text` (other
/// layout) are real words. Returns `true` when the other reading is decisively
/// more common and should win. Conservative — it fires only when the other
/// reading is a common word AND the current reading is either absent from the
/// frequency list or several times rarer. With empty frequency lists (as in unit
/// tests) it always returns `false`, so the prior "keep current layout"
/// behaviour is unchanged.
///
/// `run` is what the words before this one were written in. A run already in the
/// other language loosens both thresholds: the frequency evidence is then
/// corroborating something rather than carrying the whole decision on its own.
fn other_decisively_more_common(
    cur_text: &str,
    current: Language,
    oth_text: &str,
    other: Language,
    run: Run,
    en_freq: Freq,
    he_freq: Freq,
) -> bool {
    if !Config::global().freq_enabled {
        return false;
    }
    let (common_max, rarer_factor) = if run.favours(other) {
        (FREQ_COMMON_MAX_RUN, FREQ_RARER_FACTOR_RUN)
    } else {
        (FREQ_COMMON_MAX, FREQ_RARER_FACTOR)
    };
    let Some(oth_rank) = freq_rank(oth_text, other, en_freq, he_freq) else {
        return false; // the other reading isn't even a common word — don't override.
    };
    if oth_rank > common_max {
        return false;
    }
    match freq_rank(cur_text, current, en_freq, he_freq) {
        // Current reading is also ranked: switch only if the other is many times more common.
        Some(cur_rank) => cur_rank >= oth_rank.saturating_mul(rarer_factor),
        // Current reading is absent from the top-N list while the other is very
        // common: the common reading almost certainly wins.
        None => true,
    }
}

/// Longest reading the short-word gate has an opinion about.
const SHORT_MAX: usize = 3;

/// Automatic corrections need at least two letters of actual word text.
///
/// A lone key is too ambiguous to rewrite: it may be an unfinished word, a
/// shortcut key, or one of the one-letter words that exists only in the other
/// layout. Dictionary membership alone must never turn a one-character token
/// into a character from the other layout. User-configured abbreviations remain
/// allowed because they are explicit instructions rather than an inferred
/// correction.
const MIN_AUTOMATIC_WORD_CHARS: usize = 2;

/// A short reading ranked this common is not a collision.
///
/// The top of a frequency list is where the words people actually mistype the
/// layout of live — `the`, `and`, `של`, `זה`. Two or three letters of one of
/// those is not the accidental dictionary hit the gate exists to suppress.
const SHORT_COMMON_MAX: u32 = 500;

/// Whether `text`, read as `lang`, is too short to be believed as a trigger.
///
/// Only has an opinion when the user has turned short-word switching off
/// (`short_enabled`, from `RECAST_SHORT=0`), and length is not the whole of the
/// answer even then. Length was always a proxy for "this is probably an
/// accidental dictionary collision rather than a word anyone meant", and the
/// frequency list answers that question directly: a two-letter reading nobody
/// types is the collision the gate is for, and a two-letter reading everybody
/// types is not.
///
/// `short_enabled` is passed rather than read from the global config so the
/// gate can be tested at all — the global is a `OnceLock` holding the shipped
/// default, and every test would otherwise exercise only the disabled path.
fn too_short_to_trigger(
    text: &str,
    lang: Language,
    short_enabled: bool,
    en_freq: Freq,
    he_freq: Freq,
) -> bool {
    if short_enabled {
        return false;
    }
    if text.chars().count() > SHORT_MAX {
        return false;
    }
    freq_rank(text, lang, en_freq, he_freq).is_none_or(|rank| rank > SHORT_COMMON_MAX)
}

/// One-letter inflectional prefixes that Hebrew attaches to nouns/verbs:
/// ו (and), ה (the), ל (to/for), ב (in), כ (as/like), מ (from), ש (that).
const HE_PREFIXES: &[char] = &['ו', 'ה', 'ל', 'ב', 'כ', 'מ', 'ש'];

/// The prefix *pairs* Hebrew actually stacks, outermost letter first.
///
/// Hebrew does not stack these freely, so this is a whitelist rather than a
/// loop: the definite article ה contracts into a preceding ב/ל/כ (ב+ה+בית is
/// written בבית, never בהבית), and ה is innermost anyway and never sits over
/// another prefix. What is genuinely written is the conjunction ו over any of
/// the others, the relativiser ש over the simple prepositions, כש, and מ over
/// ה or ש.
///
/// Capped at two. Three-letter stacks exist (לכשה־) but they are rare enough
/// that the recall is not worth widening a *guard* for, and the dictionary
/// already holds many prefixed forms outright.
const HE_PREFIX_PAIRS: &[&str] = &[
    "וה", "ול", "וב", "וכ", "ומ", "וש", "שה", "של", "שב", "שכ", "שמ", "כש", "מה", "מש",
];

/// Shortest stem two stripped prefixes may leave behind. A single letter is not
/// a word anyone was writing — it is what is left over when the stripping was
/// wrong, and matching on it would make the guard fire on almost anything
/// starting with two of these letters.
const HE_STEM_MIN: usize = 2;

/// Hebrew lookup with a prefix fallback: if the word is not in the dict
/// directly, try stripping the inflectional prefixes off the front and looking
/// up the stem.
///
/// One prefix is stripped unconditionally; a second only for the pairs in
/// [`HE_PREFIX_PAIRS`], because stacking is what Hebrew does but not with every
/// combination. Over-matching here is the safe direction — this is the *guard*
/// test, so a word it recognises is a word left alone rather than a word
/// rewritten.
fn matches_hebrew(word: &str, dict: Dict) -> bool {
    if dict.contains(word) {
        return true;
    }
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !HE_PREFIXES.contains(&first) {
        return false;
    }
    let rest = chars.as_str();
    if !rest.is_empty() && dict.contains(rest) {
        return true;
    }
    let mut rest_chars = rest.chars();
    let Some(second) = rest_chars.next() else {
        return false;
    };
    let stem = rest_chars.as_str();
    let pair: String = [first, second].into_iter().collect();
    HE_PREFIX_PAIRS.contains(&pair.as_str())
        && stem.chars().count() >= HE_STEM_MIN
        && dict.contains(stem)
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
/// Note: the guard is deliberately *strict*, not loose — a looked-up word is
/// better evidence than an inferred one. But a prefixed Hebrew form whose keys
/// also spell a real English word is not thereby worthless: it used to lose
/// outright, and now it costs the English reading a frequency contest to win
/// (see `a_prefixed_hebrew_form_only_loses_to_a_common_english_word`).
#[allow(clippy::too_many_arguments)]
fn decide_known(
    text_en: &str,
    text_he: &str,
    current: Language,
    run: Run,
    en_dict: Dict,
    he_dict: Dict,
    en_freq: Freq,
    he_freq: Freq,
) -> Option<Language> {
    let other = current.other();
    let cur_text = if current == Language::English {
        text_en
    } else {
        text_he
    };
    let oth_text = if other == Language::English {
        text_en
    } else {
        text_he
    };

    // Single-character inputs are only corrected if they are real dictionary
    // words (e.g. "i", "a" in English). This prevents stray single keystrokes
    // like "r" on Hebrew layout from triggering a layout switch.
    if oth_text.chars().count() == 1 && !valid_strict(oth_text, other, en_dict, he_dict) {
        return None;
    }

    let oth_strict = valid_strict(oth_text, other, en_dict, he_dict);
    // Short-word gate: short words are dictionary-collision-prone; when disabled
    // (RECAST_SHORT=0) an other-layout reading of that length never triggers a
    // switch — neither the plain trigger nor the frequency tie-break — unless it
    // is common enough that the collision reading does not hold up.
    let short_block = too_short_to_trigger(
        oth_text,
        other,
        Config::global().short_enabled,
        en_freq,
        he_freq,
    );

    // Guard: the current layout already forms a strict word (including the
    // homograph case where both layouts do) → preserve user intent, unless the
    // other reading is decisively more common (frequency tie-break).
    if valid_strict(cur_text, current, en_dict, he_dict) {
        if oth_strict
            && !short_block
            && other_decisively_more_common(
                cur_text, current, oth_text, other, run, en_freq, he_freq,
            )
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
        // …unless the current reading is a *loose* match — a Hebrew form the
        // prefix rules recognise without the dictionary holding it outright.
        // That is weaker evidence than a strict hit, since it is inferred
        // rather than looked up, but it is a long way from nothing: switching
        // on top of it rewrites a real Hebrew word into an unrelated English
        // one, which is the most damaging thing this function can do. So the
        // English reading has to win a frequency contest first, and the run
        // counts towards it exactly as it does for a homograph above.
        //
        // Only Hebrew can reach this: English has no inflectional prefixes, so
        // its loose test is its strict one and the guard above already fired.
        if valid_loose(cur_text, current, en_dict, he_dict)
            && !other_decisively_more_common(
                cur_text, current, oth_text, other, run, en_freq, he_freq,
            )
        {
            return None;
        }
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
    run: Run,
    en_dict: Dict,
    he_dict: Dict,
    en_freq: Freq,
    he_freq: Freq,
) -> Option<Language> {
    // Single-character inputs are only corrected if they are real dictionary
    // words (e.g. "i", "a" in English). This prevents stray single keystrokes
    // from triggering a layout switch when the layout is unknown.
    if text_en.chars().count() == 1 && !valid_strict(text_en, Language::English, en_dict, he_dict) {
        return None;
    }
    if text_he.chars().count() == 1 && !valid_strict(text_he, Language::Hebrew, en_dict, he_dict) {
        return None;
    }

    // Short-word gate: when disabled, a short uncommon reading never counts as a
    // trigger — the same collision guard as in `decide_known`.
    let enabled = Config::global().short_enabled;
    let short_ok = |text: &str, lang| !too_short_to_trigger(text, lang, enabled, en_freq, he_freq);
    let en_strict = short_ok(text_en, Language::English)
        && valid_strict(text_en, Language::English, en_dict, he_dict);
    let he_strict = short_ok(text_he, Language::Hebrew)
        && valid_strict(text_he, Language::Hebrew, en_dict, he_dict);
    // If exactly one layout has a strict match, switch to that layout.
    if en_strict && !he_strict {
        return Some(Language::English);
    } else if he_strict && !en_strict {
        return Some(Language::Hebrew);
    } else if en_strict && he_strict {
        // Both layouts read as words: break the tie by frequency (and by the
        // run, which is usually the stronger of the two), else leave it alone.
        // The winner must be decisively more common than the loser.
        if other_decisively_more_common(
            text_he,
            Language::Hebrew,
            text_en,
            Language::English,
            run,
            en_freq,
            he_freq,
        ) {
            return Some(Language::English);
        } else if other_decisively_more_common(
            text_en,
            Language::English,
            text_he,
            Language::Hebrew,
            run,
            en_freq,
            he_freq,
        ) {
            return Some(Language::Hebrew);
        }
    }
    None
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
/// A correction may be produced by one pipeline or by a composition of them
/// (see [`plan`]). In particular, a misspelled English word typed while the
/// Hebrew layout is active needs both a layout switch and a spelling rewrite.
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
    /// The same keystrokes are a plausible English misspelling under the other
    /// layout. The layout has already been switched to `lang`; unlike a plain
    /// layout fix, callers must type `text` rather than replaying the original
    /// keys because those keys still spell the uncorrected word.
    LayoutSpelling { text: String, lang: Language },
    /// Rewrite in the current (English) layout, from the speller or from an
    /// abbreviation expansion: the layout is untouched and the caller should
    /// erase the whole word and type `text` instead. `text` is ASCII and
    /// typeable through the English keymap, but — unlike before case tracking —
    /// it may contain capitals, and an expansion may contain spaces.
    Spelling { text: String },
}

/// Everything a finished word produced: what to do about it, and what language
/// it turned out to be.
///
/// The second half is not for this word — it is for the next one. The listener
/// feeds it into [`History`], and the run that comes back out is what lets a
/// genuinely ambiguous key sequence be resolved by what was being written around
/// it rather than by frequency tables alone.
pub struct Outcome {
    /// The correction to apply, or `None` to leave the word alone.
    pub fix: Option<Fix>,
    /// The language the word turned out to be, when that is knowable at all.
    pub lang: Option<Language>,
}

/// The language a key sequence is in, when the dictionaries agree on an answer.
///
/// `None` for a sequence that reads as a word in both layouts (which says
/// nothing) and for one that reads as a word in neither (a name, a handle, a
/// typo — also nothing). Only an unambiguous reading is evidence, and evidence
/// is all the run is allowed to be built from.
fn observed(word_en: &str, word_he: &str, en_dict: Dict, he_dict: Dict) -> Option<Language> {
    let en = valid_loose(word_en, Language::English, en_dict, he_dict);
    let he = valid_loose(word_he, Language::Hebrew, en_dict, he_dict);
    match (en, he) {
        (true, false) => Some(Language::English),
        (false, true) => Some(Language::Hebrew),
        _ => None,
    }
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
    Switch {
        lang: Language,
        start: usize,
    },
    Spell {
        text: String,
    },
    SwitchAndSpell {
        lang: Language,
        text: String,
    },
    /// A user-configured abbreviation was typed out in full.
    Expand {
        text: String,
    },
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

/// Whether a learned replacement is English text (ignoring spaces and
/// punctuation). This lets a remembered composed correction keep switching to
/// English on later occurrences instead of replaying English letters through
/// the still-active Hebrew layout.
fn is_english_text(text: &str) -> bool {
    let mut letters = text.chars().filter(|c| c.is_alphabetic());
    letters.next().is_some_and(|first| {
        first.is_ascii_alphabetic() && letters.all(|c| c.is_ascii_alphabetic())
    })
}

fn debug_log(word_en: &str, word_he: &str, target: Option<Language>, switched: bool) {
    if !debug_enabled() {
        return;
    }
    println!("{}", word_en);
    println!("{}", word_he);
    println!(
        "English: {}",
        if matches!(target, Some(Language::English)) {
            "True"
        } else {
            "False"
        }
    );
    println!(
        "Hebrew: {}",
        if matches!(target, Some(Language::Hebrew)) {
            "True"
        } else {
            "False"
        }
    );
    println!("Switch: {}", if switched { "True" } else { "False" });
}

/// Pure planning step: decide what to do with the word, given the folded
/// buffers, the per-key offset tables, and the live `current` layout. Kept free
/// of any I/O so it is unit testable; the actual layout switch happens in the
/// caller.
///
/// The pipelines run in a fixed order, but may compose into one atomic plan:
///
///   0. **Abbreviation expansion** (English only). The user wrote this rule
///      down themselves, so nothing gets to overrule it.
///   1. **Layout** (whole buffer, then the opt-in missing-space split). It goes
///      ahead of the speller because it is the *exact* signal: the keystrokes
///      literally spell a real word in the other language, no guessing involved.
///   2. **Spelling**. In an English layout it rewrites the current reading. In
///      a Hebrew layout it checks the alternate English reading; a confident
///      correction becomes one switch-and-spell operation. The current Hebrew
///      reading must still be unknown, so a real Hebrew word always wins.
#[allow(clippy::too_many_arguments)]
fn plan(
    en: Reading,
    he: Reading,
    offsets_en: &[usize],
    offsets_he: &[usize],
    keys_len: usize,
    current: Option<Language>,
    run: Run,
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

    // Personal confusion pair: the user has explicitly corrected this word
    // before (via undo or post-fix edit). This outranks everything — it's
    // their deliberate choice.
    if let Some(correction) = crate::personal::personal_correction(typed) {
        if current == Some(Language::Hebrew) && is_english_text(&correction) {
            return Some(Plan::SwitchAndSpell {
                lang: Language::English,
                text: correction,
            });
        }
        return Some(Plan::Spell { text: correction });
    }

    // An expansion the user configured by hand outranks everything we infer.
    if current == Some(Language::English) {
        if let Some(text) = crate::complete::expand(word_en) {
            return Some(Plan::Expand { text });
        }
    }

    // Do not infer a layout or spelling correction from a single typed letter.
    // Check the word portions, rather than `keys_len`, so trailing punctuation
    // cannot make a one-letter word look long enough.
    if word_en.chars().count() < MIN_AUTOMATIC_WORD_CHARS
        || word_he.chars().count() < MIN_AUTOMATIC_WORD_CHARS
    {
        return None;
    }

    // Whole-buffer decision first — this is what fires for virtually every real
    // correction.
    let whole = match current {
        Some(cur) => decide_known(
            word_en, word_he, cur, run, en_dict, he_dict, en_freq, he_freq,
        ),
        None => decide_unknown(word_en, word_he, run, en_dict, he_dict, en_freq, he_freq),
    };
    if let Some(lang) = whole {
        return Some(Plan::Switch { lang, start: 0 });
    }

    if let Some(split) = plan_split(
        en.full, he.full, offsets_en, offsets_he, keys_len, current, run, en_dict, he_dict,
        en_freq, he_freq,
    ) {
        return Some(split);
    }

    plan_spelling(word_en, word_he, current, case, en_dict, he_dict, en_freq)
}

/// Second pipeline: neither exact layout reading matched, but the English
/// reading may be a misspelling.
///
/// With a known English layout this is an ordinary spelling rewrite. With a
/// known Hebrew layout, a confident English correction composes with a switch
/// to English. An unknown live layout remains hands-off because we cannot know
/// whether a switch is required.
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
    let current = current?;
    if case == Case::Upper {
        return None;
    }
    // A word the user has declared theirs is never second-guessed — whether
    // they declared it by writing it in `ignore.txt` or by taking the same
    // correction back twice.
    if crate::complete::ignored(full_en) || crate::complete::learned(full_en) {
        return None;
    }
    if valid_loose(full_he, Language::Hebrew, en_dict, he_dict) {
        return None;
    }
    let text = crate::spell::correct(full_en, en_dict, en_freq)?;
    Some(compose_spelling(current, text))
}

/// Turn any accepted English spelling result into the action required by the
/// live layout. This step deliberately has no access to the typed word: once
/// the speller accepts a candidate, composition is a rule of layout context and
/// cannot grow per-word exceptions.
fn compose_spelling(current: Language, text: String) -> Plan {
    match current {
        Language::English => Plan::Spell { text },
        Language::Hebrew => Plan::SwitchAndSpell {
            lang: Language::English,
            text,
        },
    }
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
    run: Run,
    en_dict: Dict,
    he_dict: Dict,
    freq_en: Freq,
    freq_he: Freq,
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
            Language::English => (&full_en[..offsets_en[split]], &full_en[offsets_en[split]..]),
            Language::Hebrew => (&full_he[..offsets_he[split]], &full_he[offsets_he[split]..]),
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
        let enabled = Config::global().short_enabled;
        if too_short_to_trigger(oth_suffix, other, enabled, freq_en, freq_he) {
            continue;
        }
        if valid_strict(oth_suffix, other, en_dict, he_dict)
            && !valid_loose(cur_suffix, current, en_dict, he_dict)
        {
            // Confirm split only when the other suffix is decisively the common
            // reading (frequency tie-break), matching the main pipeline.
            let full_cur = match current {
                Language::English => full_en,
                Language::Hebrew => full_he,
            };
            if !other_decisively_more_common(
                full_cur, current, oth_suffix, other, run, freq_en, freq_he,
            ) {
                continue;
            }
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
/// switch when the *other* layout yields a confident dictionary word. If that
/// other reading is instead a confident English misspelling, layout switching
/// and spelling correction are applied together. A missing-space split fallback
/// exists but is opt-in (`RECAST_SPLIT=1`) because it cannot be made reliably
/// safe.
///
/// Returns the single [`Fix`] to apply — or `None` to leave the word alone —
/// alongside the language the word turned out to be, which the caller records
/// so the *next* word can be decided with a run behind it. For `Fix::Layout` the
/// layout switch has already happened by the time this returns (and no fix is
/// returned if the OS refused it); [`Fix::LayoutSpelling`] likewise switches
/// first but replaces the word with corrected text rather than replayed keys.
/// For `Fix::Spelling` no layout call is made at all.
///
/// `run` is the language of the words immediately before this one — see
/// [`History`].
#[allow(clippy::too_many_arguments)]
pub fn check_and_correct<K: Copy>(
    keys: &[K],
    to_en: impl Fn(K) -> Option<char>,
    to_he: impl Fn(K) -> Option<char>,
    shift_of: impl Fn(K) -> bool,
    run: Run,
    en_dict: Dict,
    he_dict: Dict,
    current: Option<Language>,
    switch_layout_to: impl Fn(Language) -> crate::layout::LayoutSwitch,
) -> Outcome {
    if keys.is_empty() {
        return Outcome {
            fix: None,
            lang: None,
        };
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

    // Track layout query failures for dead-backend detection.
    if current.is_none() {
        crate::layout::record_layout_failure();
    } else {
        crate::layout::reset_layout_failures();
    }
    // What the word says about the language being written, independent of
    // whatever is about to be done to it. A layout switch overrides this below
    // — that decision is the better answer — but for every word the pipelines
    // leave alone, this is the only answer there is.
    let seen = observed(en.word, he.word, en_dict, he_dict);
    let Some(plan) = plan(
        en,
        he,
        &offsets_en,
        &offsets_he,
        keys.len(),
        current,
        run,
        case,
        en_dict,
        he_dict,
        en_freq(),
        he_freq(),
    ) else {
        debug_log(&full_en, &full_he, None, false);
        return Outcome {
            fix: None,
            lang: seen,
        };
    };

    let (fix, lang) = match plan {
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
            //
            // The word is still recorded as being in the target language either
            // way. That is what the pipeline concluded about it, and a refusal
            // by the compositor is a fact about the compositor rather than about
            // what the user was writing.
            (
                switched.then_some(Fix::Layout { start, text, lang }),
                Some(lang),
            )
        }
        Plan::Spell { text } | Plan::Expand { text } => {
            // No layout call at all — the word stays in English, only its
            // letters change.
            debug_log(&full_en, &full_he, None, false);
            if debug_enabled() {
                println!("spell: {} -> {}{}", full_en, text, en.tail);
            }
            (
                Some(Fix::Spelling {
                    text: respelled(&text, case, en.tail),
                }),
                Some(Language::English),
            )
        }
        Plan::SwitchAndSpell { lang, text } => {
            let switched = switch_layout_to(lang).changed();
            debug_log(&full_en, &full_he, Some(lang), switched);
            let text = respelled(&text, case, en.tail);
            if debug_enabled() {
                println!("layout+spell: {} -> {}", full_he, text);
            }
            (
                switched.then_some(Fix::LayoutSpelling { text, lang }),
                Some(lang),
            )
        }
    };

    Outcome { fix, lang }
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
    current: Option<Language>,
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
    let typed = match current {
        Some(Language::Hebrew) => &full_he[..word_end_he],
        _ => &full_en[..word_end_en],
    };
    if crate::complete::suppressed(typed) {
        return Some(typed.to_string());
    }
    let word_en = &full_en[..word_end_en];
    if current == Some(Language::English)
        && (crate::complete::ignored(word_en) || crate::complete::learned(word_en))
    {
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
    current: Option<Language>,
) -> Vec<String> {
    if keys.is_empty() || current != Some(Language::English) {
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
    fn correction_accuracy_corpus() {
        // Exercise the planner with explicit layouts: this check never switches
        // the OS keyboard or depends on whichever layout the developer uses.
        let mut unchanged = 0;
        let mut wanted = 0;
        let mut unwanted = Vec::new();
        let mut missed = Vec::new();
        let mut wrong = Vec::new();
        for (line, row) in include_str!("../tests/data/corrections.tsv")
            .lines()
            .enumerate()
        {
            if row.is_empty() || row.starts_with('#') {
                continue;
            }
            let fields: Vec<_> = row.split('\t').collect();
            assert_eq!(
                fields.len(),
                4,
                "corpus line {} must have four columns",
                line + 1
            );
            let current = match fields[0] {
                "en" => Language::English,
                "he" => Language::Hebrew,
                other => panic!("invalid layout {other}"),
            };
            let (en_text, he_text, expected) = (fields[1], fields[2], fields[3]);
            let before = if current == Language::English {
                en_text
            } else {
                he_text
            };
            assert_eq!(
                en_text.chars().count(),
                he_text.chars().count(),
                "corpus line {} has mismatched key counts",
                line + 1
            );
            let keys: Vec<_> = en_text
                .chars()
                .zip(he_text.chars())
                .map(|(en, he)| {
                    (
                        en.to_ascii_lowercase(),
                        he,
                        en.is_ascii_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(en),
                    )
                })
                .collect();
            let result = check_and_correct(
                &keys,
                |k| Some(k.0),
                |k| Some(k.1),
                |k| k.2,
                Run::default(),
                en_dict(),
                he_dict(),
                Some(current),
                |_| crate::layout::LayoutSwitch::Switched,
            );
            let after = match result.fix {
                None => before.to_string(),
                Some(
                    Fix::Layout { text, start: 0, .. }
                    | Fix::Spelling { text }
                    | Fix::LayoutSpelling { text, .. },
                ) => text,
                Some(Fix::Layout { start, .. }) => {
                    panic!("unexpected split at {start}: splitting is disabled")
                }
            };
            let detail = format!(
                "line {}: {before:?} -> {after:?}; expected {expected:?}",
                line + 1
            );
            if expected == before {
                unchanged += 1;
                if after != before {
                    unwanted.push(detail);
                }
            } else {
                wanted += 1;
                if after == before {
                    missed.push(detail);
                } else if after != expected {
                    wrong.push(detail);
                }
            }
        }
        assert!(
            unchanged > 0 && wanted > 0,
            "the corpus must test both protection and correction"
        );
        eprintln!(
            "Accuracy: unwanted {}/{unchanged}; missed {}/{wanted}; wrong {}/{wanted}",
            unwanted.len(),
            missed.len(),
            wrong.len()
        );
        assert!(unwanted.is_empty() && missed.is_empty() && wrong.is_empty(),
            "unwanted changes: {unwanted:#?}\nmissed corrections: {missed:#?}\nwrong corrections: {wrong:#?}");
    }

    #[test]
    fn binary_search_finds_every_entry_and_nothing_else() {
        // The lookup walks back from an arbitrary probe byte to a line start,
        // so exercise it against a list long enough to need several probes.
        let words = [
            "a", "aa", "ab", "abc", "b", "hello", "helot", "zebra", "שלום",
        ];
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
        assert_eq!(
            decide_known(
                "hello",
                "ימךךם",
                Language::English,
                Run::default(),
                en,
                he,
                nofreq(),
                nofreq()
            ),
            None
        );
        // Typing "שלום" while in Hebrew layout: do nothing.
        assert_eq!(
            decide_known(
                "akuo",
                "שלום",
                Language::Hebrew,
                Run::default(),
                en,
                he,
                nofreq(),
                nofreq()
            ),
            None
        );
    }

    #[test]
    fn wrong_layout_switches_to_other() {
        let en = dict(&["hello"]);
        let he = dict(&["שלום"]);
        // In Hebrew layout but the keys spell "hello" in English -> switch EN.
        assert_eq!(
            decide_known(
                "hello",
                "ימךךם",
                Language::Hebrew,
                Run::default(),
                en,
                he,
                nofreq(),
                nofreq()
            ),
            Some(Language::English)
        );
        // In English layout but the keys spell "שלום" in Hebrew -> switch HE.
        assert_eq!(
            decide_known(
                "akuo",
                "שלום",
                Language::English,
                Run::default(),
                en,
                he,
                nofreq(),
                nofreq()
            ),
            Some(Language::Hebrew)
        );
    }

    #[test]
    fn prefixed_hebrew_is_not_mangled() {
        // "שלום" is in the dict; "ושלום" not, but matches via one‑letter prefix.
        let en = dict(&["hello"]);
        let he = dict(&["שלום"]);
        assert_eq!(
            decide_known(
                "uakuo",
                "ושלום",
                Language::Hebrew,
                Run::default(),
                en,
                he,
                nofreq(),
                nofreq()
            ),
            None
        );
    }

    #[test]
    fn fun_word_mistyped_in_hebrew_switches() {
        let en = dict(&["fun"]);
        let he = dict(&[]);
        // Hebrew reading = "כום"
        assert_eq!(
            decide_known(
                "fun",
                "כום",
                Language::Hebrew,
                Run::default(),
                en,
                he,
                nofreq(),
                nofreq()
            ),
            Some(Language::English)
        );
    }

    #[test]
    fn very_word_mistyped_in_hebrew_switches() {
        let en = dict(&["very"]);
        let he = dict(&[]);
        // Hebrew reading = "הקרט"
        assert_eq!(
            decide_known(
                "very",
                "הקרט",
                Language::Hebrew,
                Run::default(),
                en,
                he,
                nofreq(),
                nofreq()
            ),
            Some(Language::English)
        );
    }

    #[test]
    fn a_prefixed_hebrew_form_only_loses_to_a_common_english_word() {
        // "ושלום" is a real Hebrew word the dictionary only knows through the
        // prefix rule, and the same keys spell a real English word. The English
        // reading is a strict dictionary hit and the Hebrew one is inferred, so
        // English is the better-evidenced reading — but not by enough to rewrite
        // a Hebrew word on its own.
        let en = dict(&["uakuo"]);
        let he = dict(&["שלום"]);
        // Nothing known about how common the English word is: leave it alone.
        assert_eq!(
            decide_known(
                "uakuo",
                "ושלום",
                Language::Hebrew,
                Run::default(),
                en,
                he,
                nofreq(),
                nofreq()
            ),
            None
        );
        // A genuinely common English word does win.
        let en_f = freq(&[("uakuo", 300)]);
        assert_eq!(
            decide_known(
                "uakuo",
                "ושלום",
                Language::Hebrew,
                Run::default(),
                en,
                he,
                en_f,
                nofreq()
            ),
            Some(Language::English)
        );
        // And a rare one does not, however real it is.
        let rare = freq(&[("uakuo", 30_000)]);
        assert_eq!(
            decide_known(
                "uakuo",
                "ושלום",
                Language::Hebrew,
                Run::default(),
                en,
                he,
                rare,
                nofreq()
            ),
            None
        );
        // A run of English words is the other way to pay for it: the same rare
        // word, arriving in the middle of English text.
        let run = Run {
            lang: Some(Language::English),
            len: 3,
        };
        assert_eq!(
            decide_known(
                "uakuo",
                "ושלום",
                Language::Hebrew,
                run,
                en,
                he,
                rare,
                nofreq()
            ),
            None,
            "30000 is past even the run-relaxed FREQ_COMMON_MAX_RUN"
        );
        let middling = freq(&[("uakuo", 9_000)]);
        assert_eq!(
            decide_known(
                "uakuo",
                "ושלום",
                Language::Hebrew,
                run,
                en,
                he,
                middling,
                nofreq()
            ),
            Some(Language::English)
        );
        assert_eq!(
            decide_known(
                "uakuo",
                "ושלום",
                Language::Hebrew,
                Run::default(),
                en,
                he,
                middling,
                nofreq()
            ),
            None,
            "the same word without the run behind it is not enough"
        );
    }

    #[test]
    fn an_unprefixed_hebrew_collision_still_switches() {
        // The softening above is only about the *inferred* prefix match. A key
        // sequence the Hebrew dictionary does not recognise at all, prefix rule
        // included, is still a plain wrong-layout mistype.
        let en = dict(&["uakuo"]);
        let he = dict(&["שלום"]);
        assert_eq!(
            decide_known(
                "uakuo",
                "זזזזז",
                Language::Hebrew,
                Run::default(),
                en,
                he,
                nofreq(),
                nofreq()
            ),
            Some(Language::English)
        );
    }

    #[test]
    fn short_gibberish_never_switches() {
        // Short sequence that is a word in NEITHER dict must not trigger a
        // switch, regardless of the short-word config.
        let en = dict(&[]);
        let he = dict(&[]);
        assert_eq!(
            decide_known(
                "xkc",
                "סלב",
                Language::English,
                Run::default(),
                en,
                he,
                nofreq(),
                nofreq()
            ),
            None
        );
        assert_eq!(
            decide_unknown("xkc", "סלב", Run::default(), en, he, nofreq(), nofreq()),
            None
        );
    }

    #[test]
    fn every_one_character_token_skips_automatic_correction() {
        // A one-character token can have a dictionary reading in the other
        // layout, but it is too ambiguous to rewrite. Exercise each direction
        // of the layout decision without giving either character special status.
        assert_eq!(
            plan_for(
                "r",
                "ה",
                Some(Language::Hebrew),
                dict(&["r"]),
                dict(&[]),
                nofreq(),
            ),
            None
        );
        assert_eq!(
            plan_for(
                "r",
                "ה",
                Some(Language::English),
                dict(&[]),
                dict(&["ה"]),
                nofreq(),
            ),
            None
        );
    }

    #[test]
    fn unknown_layout_switches_only_on_one_sided_match() {
        let en = dict(&["hello"]);
        let he = dict(&["שלום"]);
        assert_eq!(
            decide_unknown("hello", "ימךךם", Run::default(), en, he, nofreq(), nofreq()),
            Some(Language::English)
        );
        assert_eq!(
            decide_unknown("akuo", "שלום", Run::default(), en, he, nofreq(), nofreq()),
            Some(Language::Hebrew)
        );
        // In neither dict → no switch.
        assert_eq!(
            decide_unknown("qqqq", "ננננ", Run::default(), en, he, nofreq(), nofreq()),
            None
        );
    }

    #[test]
    fn ambiguous_homograph_never_switches() {
        // Keys valid as a word in BOTH layouts: do not switch, preserve user intent.
        let en = dict(&["go"]);
        let he = dict(&["עט"]); // whatever the keys read as in Hebrew
        assert_eq!(
            decide_known(
                "go",
                "עט",
                Language::English,
                Run::default(),
                en,
                he,
                nofreq(),
                nofreq()
            ),
            None
        );
        assert_eq!(
            decide_known(
                "go",
                "עט",
                Language::Hebrew,
                Run::default(),
                en,
                he,
                nofreq(),
                nofreq()
            ),
            None
        );
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
            decide_known(
                "go",
                "עט",
                Language::Hebrew,
                Run::default(),
                en,
                he,
                en_f,
                he_f
            ),
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
            decide_known(
                "go",
                "עט",
                Language::Hebrew,
                Run::default(),
                en,
                he,
                en_f,
                he_f
            ),
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
            Run::default(),
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
            Some(Plan::Spell {
                text: "hello".to_string()
            })
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
            Some(Plan::Switch {
                lang: Language::Hebrew,
                start: 0
            })
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
        assert_eq!(
            plan,
            Some(Plan::Spell {
                text: "hello".to_string()
            })
        );
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
    fn wrong_layout_misspellings_stack_for_every_letter_shape() {
        // Generate a family of unrelated candidates. Every typo drops half of
        // a doubled letter, which is one generic channel rule. The planner must
        // compose every accepted result with a layout switch; no named word is
        // granted special behavior.
        for letter in b'a'..=b'z' {
            let letter = char::from(letter);
            let corrected = format!("ba{letter}{letter}er");
            let typed = format!("ba{letter}er");
            let en = dict(&[&corrected]);
            let he = dict(&[]);
            let en_f = freq(&[(&corrected, 100)]);
            assert_eq!(
                plan_for(&typed, "טקסט", Some(Language::Hebrew), en, he, en_f),
                Some(Plan::SwitchAndSpell {
                    lang: Language::English,
                    text: corrected.clone(),
                }),
                "{typed} should pass through both pipelines"
            );
        }
    }

    #[test]
    fn spelling_composition_depends_only_on_layout() {
        for len in 1..=64 {
            let candidate: String = (0..len)
                .map(|index| char::from(b'a' + (index % 26) as u8))
                .collect();
            assert_eq!(
                compose_spelling(Language::English, candidate.clone()),
                Plan::Spell {
                    text: candidate.clone(),
                }
            );
            assert_eq!(
                compose_spelling(Language::Hebrew, candidate.clone()),
                Plan::SwitchAndSpell {
                    lang: Language::English,
                    text: candidate,
                }
            );
        }
    }

    #[test]
    fn stacked_spelling_never_overrides_any_real_current_layout_word() {
        for (index, letter) in (b'a'..=b'z').enumerate() {
            let letter = char::from(letter);
            let corrected = format!("ba{letter}{letter}er");
            let typed = format!("ba{letter}er");
            let current_word = format!(
                "מ{}ה",
                char::from_u32(0x05d0 + index as u32).expect("Hebrew letter")
            );
            let en = dict(&[&corrected]);
            let he = dict(&[&current_word]);
            let en_f = freq(&[(&corrected, 100)]);

            assert_eq!(
                plan_for(&typed, &current_word, Some(Language::Hebrew), en, he, en_f,),
                None,
                "current-layout dictionary membership must win for {current_word}"
            );
        }
    }

    #[test]
    fn known_word_is_never_spell_corrected() {
        // A real English word is left alone even when a far more common word is
        // one edit away.
        let en = dict(&["form", "from"]);
        let he = dict(&[]);
        let en_f = freq(&[("from", 10), ("form", 900)]);
        assert_eq!(
            plan_for("form", "בםרצ", Some(Language::English), en, he, en_f),
            None
        );
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
            plan_cased(
                "nasa",
                "מקק",
                Some(Language::English),
                Case::Upper,
                en,
                he,
                en_f
            ),
            None
        );
        // The same letters typed normally are still fair game.
        assert_eq!(
            plan_cased(
                "nasa",
                "מקק",
                Some(Language::English),
                Case::Lower,
                en,
                he,
                en_f
            ),
            Some(Plan::Spell {
                text: "nada".to_string()
            })
        );
    }

    #[test]
    fn an_all_caps_word_is_still_layout_switched() {
        // Case only silences the *speller*: keys that literally spell a Hebrew
        // word were still typed in the wrong layout, shouting or not.
        let en = dict(&[]);
        let he = dict(&["שלום"]);
        assert_eq!(
            plan_cased(
                "akuo",
                "שלום",
                Some(Language::English),
                Case::Upper,
                en,
                he,
                nofreq()
            ),
            Some(Plan::Switch {
                lang: Language::Hebrew,
                start: 0
            })
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
            Run::default(),
            Case::Lower,
            en,
            he,
            nofreq(),
            nofreq(),
        );
        assert_eq!(
            plan,
            Some(Plan::Switch {
                lang: Language::Hebrew,
                start: 0
            })
        );
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
                Run::default(),
                Case::Lower,
                en,
                he,
                en_f,
                nofreq(),
            ),
            Some(Plan::Spell {
                text: "hello".to_string()
            })
        );
    }

    #[test]
    fn the_run_is_the_streak_at_the_end_not_the_majority() {
        let mut h = History::default();
        assert_eq!(h.run(), Run { lang: None, len: 0 });
        h.push(Language::Hebrew);
        h.push(Language::Hebrew);
        h.push(Language::Hebrew);
        assert_eq!(
            h.run(),
            Run {
                lang: Some(Language::Hebrew),
                len: 3
            }
        );
        // One English word breaks the run rather than being outvoted by the
        // three Hebrew ones behind it.
        h.push(Language::English);
        assert_eq!(
            h.run(),
            Run {
                lang: Some(Language::English),
                len: 1
            }
        );
    }

    #[test]
    fn the_run_forgets_the_distant_past_and_a_moved_cursor() {
        let mut h = History::default();
        for _ in 0..RUN_MEMORY * 3 {
            h.push(Language::Hebrew);
        }
        // Bounded: a daemon running for weeks must not accumulate one entry per
        // word for the whole of it.
        assert_eq!(h.recent.len(), RUN_MEMORY);
        assert_eq!(h.run().len as usize, RUN_MEMORY);
        h.clear();
        assert_eq!(h.run(), Run { lang: None, len: 0 });
    }

    #[test]
    fn only_an_unambiguous_reading_counts_as_evidence() {
        let en = dict(&["hello"]);
        let he = dict(&["שלום"]);
        assert_eq!(observed("hello", "ימךךם", en, he), Some(Language::English));
        assert_eq!(observed("akuo", "שלום", en, he), Some(Language::Hebrew));
        // A word in both layouts says nothing about which one is being written…
        let both_en = dict(&["go"]);
        let both_he = dict(&["עט"]);
        assert_eq!(observed("go", "עט", both_en, both_he), None);
        // …and neither does a word in neither: a name or a typo is not evidence
        // about the language, and guessing one would make the run noise.
        assert_eq!(observed("qqqq", "ננננ", en, he), None);
    }

    #[test]
    fn a_run_in_the_other_language_resolves_a_homograph() {
        // Both readings are real words, and the frequency gap is nowhere near
        // the FREQ_RARER_FACTOR the tie-break demands on its own — "go" is only
        // three times commoner than "עט", and well outside the top 2000.
        let en = dict(&["go"]);
        let he = dict(&["עט"]);
        let en_f = freq(&[("go", 6_000)]);
        let he_f = freq(&[("עט", 18_000)]);
        // Typing in Hebrew with no history: the user's own layout wins, which
        // is the behaviour every other test here pins.
        assert_eq!(
            decide_known(
                "go",
                "עט",
                Language::Hebrew,
                Run::default(),
                en,
                he,
                en_f,
                he_f
            ),
            None
        );
        // The same word after two English words is not ambiguous any more.
        let run = Run {
            lang: Some(Language::English),
            len: 2,
        };
        assert_eq!(
            decide_known("go", "עט", Language::Hebrew, run, en, he, en_f, he_f),
            Some(Language::English)
        );
        // A run in the layout the user is already in argues for nothing — it
        // agrees with the guard, which was going to keep the word anyway.
        let same = Run {
            lang: Some(Language::Hebrew),
            len: 5,
        };
        assert_eq!(
            decide_known("go", "עט", Language::Hebrew, same, en, he, en_f, he_f),
            None
        );
    }

    #[test]
    fn one_word_is_not_a_run() {
        // The run is built out of the pipelines' own conclusions, so a single
        // word must not be enough to justify the next one — otherwise one wrong
        // call licenses the one after it.
        let en = dict(&["go"]);
        let he = dict(&["עט"]);
        let en_f = freq(&[("go", 6_000)]);
        let he_f = freq(&[("עט", 18_000)]);
        let one = Run {
            lang: Some(Language::English),
            len: 1,
        };
        assert_eq!(
            decide_known("go", "עט", Language::Hebrew, one, en, he, en_f, he_f),
            None
        );
    }

    #[test]
    fn a_run_never_overrides_a_word_the_other_layout_does_not_know() {
        // The run loosens the *tie-break* between two real words. It is not a
        // licence to switch on a reading that is not a word at all, which is
        // what would turn a name typed mid-sentence into gibberish.
        let en = dict(&["hello"]);
        let he = dict(&["שלום"]);
        let run = Run {
            lang: Some(Language::Hebrew),
            len: 5,
        };
        assert_eq!(
            decide_known(
                "qqqq",
                "ננננ",
                Language::English,
                run,
                en,
                he,
                nofreq(),
                nofreq()
            ),
            None
        );
    }

    #[test]
    fn hebrew_prefixes_stack_but_only_the_way_hebrew_stacks_them() {
        let he = dict(&["בית", "שלום"]);
        // The word itself, and one prefix, as before.
        assert!(matches_hebrew("בית", he));
        assert!(matches_hebrew("הבית", he));
        assert!(matches_hebrew("ובית", he));
        // Two, for the pairs that are actually written.
        assert!(matches_hebrew("והבית", he), "and-the-house");
        assert!(matches_hebrew("ולבית", he), "and-to-house");
        assert!(matches_hebrew("כשבית", he), "when-house");
        assert!(matches_hebrew("שהשלום", he), "that-the-peace");
        // Not the pairs that are not: ל+ה contracts to plain ל in writing, so
        // "להבית" is not a form anyone types and matching it would only widen
        // the guard for nothing.
        assert!(!matches_hebrew("להבית", he));
        assert!(!matches_hebrew("בהבית", he));
        // A stem too short to be a stem, and a stem that is not a word.
        assert!(!matches_hebrew("ושב", he));
        assert!(!matches_hebrew("והספר", he));
        // Three prefixes are past what is stripped.
        assert!(!matches_hebrew("וכשהבית", he));
    }

    #[test]
    fn the_short_word_gate_asks_how_common_a_word_is_not_only_how_long() {
        let common = freq(&[("של", 40), ("go", 120)]);
        // With the gate switched on (the shipped default) it never fires.
        assert!(!too_short_to_trigger(
            "של",
            Language::Hebrew,
            true,
            nofreq(),
            common
        ));
        // Switched off, a short reading nobody types is the collision it is for…
        assert!(too_short_to_trigger(
            "סלב",
            Language::Hebrew,
            false,
            nofreq(),
            common
        ));
        assert!(too_short_to_trigger(
            "xkc",
            Language::English,
            false,
            nofreq(),
            common
        ));
        // …and a short reading everybody types is the opposite of one. This is
        // what the bare length cutoff could not tell apart: "של" and "סלב" are
        // both three characters.
        assert!(!too_short_to_trigger(
            "של",
            Language::Hebrew,
            false,
            nofreq(),
            common
        ));
        assert!(!too_short_to_trigger(
            "go",
            Language::English,
            false,
            common,
            nofreq()
        ));
        // A short word that is merely *in* the list is not enough — it has to be
        // near the top of it.
        let rare = freq(&[("עט", 9_000)]);
        assert!(too_short_to_trigger(
            "עט",
            Language::Hebrew,
            false,
            nofreq(),
            rare
        ));
        // Long readings are none of the gate's business either way.
        assert!(!too_short_to_trigger(
            "שלום",
            Language::Hebrew,
            false,
            nofreq(),
            nofreq()
        ));
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
            decide_known(
                "go",
                "עט",
                Language::Hebrew,
                Run::default(),
                en,
                he,
                en_f,
                he_f
            ),
            None
        );
    }
}
