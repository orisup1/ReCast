use std::collections::HashSet;
use std::sync::OnceLock;

use crate::layout::switch_layout_to;
use crate::types::Language;

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
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("RECAST_SPLIT").is_some())
}

/// Parse a plain-text word list (one word per line) into a `HashSet`.
///
/// For each entry, also inserts a punctuation-stripped variant (apostrophe and
/// double-quote removed) so that words like `don't` match a typed `dont` —
/// the English keymap can't produce `'`, so the original entries would
/// otherwise be unreachable.
///
/// Operates on an already-loaded string (the dictionaries are embedded via
/// `include_str!` in `main.rs`, so the binary is self-contained and can be
/// run from any working directory).
pub fn parse_dictionary(content: &str) -> HashSet<String> {
    let mut dict = HashSet::with_capacity(content.len() / 8);
    for line in content.lines() {
        let word = line.trim();
        if word.is_empty() {
            continue;
        }
        // ASCII-only lowercase: faster than Unicode `to_lowercase`. Hebrew has
        // no case, English entries are ASCII, so byte-level folding suffices.
        let lower = word.to_ascii_lowercase();
        // Drop short all-consonant ASCII entries (e.g. "nv", "bf", "kg", "mm",
        // "mfg", "brr"). The English word list is polluted with abbreviations
        // and unit symbols that collide with Hebrew words when the same key
        // sequence is read as Hebrew — typing "מה" produces "nv" under the
        // English layout, and "nv" being a dict hit made `decide_target_lang`
        // see both languages as valid and refuse to switch. Real English
        // words always contain a vowel (a/e/i/o/u/y), so length ≤ 3 with no
        // vowel is a safe pollution filter. Hebrew entries are non-ASCII and
        // are unaffected by this gate.
        if lower.len() <= 3
            && lower.is_ascii()
            && !lower.bytes().any(|b| matches!(b, b'a' | b'e' | b'i' | b'o' | b'u' | b'y'))
        {
            continue;
        }
        if lower.bytes().any(|b| b == b'\'' || b == b'"') {
            let stripped: String =
                lower.chars().filter(|c| *c != '\'' && *c != '"').collect();
            if !stripped.is_empty() {
                dict.insert(stripped);
            }
        }
        dict.insert(lower);
    }
    dict
}

/// One-letter inflectional prefixes that Hebrew attaches to nouns/verbs:
/// ו (and), ה (the), ל (to/for), ב (in), כ (as/like), מ (from), ש (that).
const HE_PREFIXES: &[char] = &['ו', 'ה', 'ל', 'ב', 'כ', 'מ', 'ש'];

/// Hebrew lookup with single-prefix fallback: if the word is not in the dict
/// directly, try stripping a leading prefix letter and looking up the rest.
/// Only one prefix is stripped to avoid over-matching; the dictionary already
/// holds many common prefixed forms as full entries.
fn matches_hebrew(word: &str, dict: &HashSet<String>) -> bool {
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
fn valid_strict(
    text: &str,
    lang: Language,
    en_dict: &HashSet<String>,
    he_dict: &HashSet<String>,
) -> bool {
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
fn valid_loose(
    text: &str,
    lang: Language,
    en_dict: &HashSet<String>,
    he_dict: &HashSet<String>,
) -> bool {
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
///   1. The keystrokes already form a real word in the **current** layout —
///      strict *or* loose (a prefixed Hebrew form counts) → trust the user, do
///      nothing. This is the user's own rule from day one: "to change a word it
///      must not mean anything in the current language." It kills both "my real
///      word got replaced" and "a nested/prefixed word got flipped" — including
///      the case where the other-layout reading is *also* a dictionary word
///      (a homograph), which we must leave to the layout the user is actually in.
///   2. Else they form a confident (strict) word in the **other** layout
///      → the user typed in the wrong layout, switch. (Fixes the actual
///      mistypes.)
///   3. Otherwise it's an unknown word (name/typo/slang) → leave it alone.
///
/// Note the ordering: the loose current-layout guard (1) is checked *before* the
/// other-layout trigger (2). Checking the trigger first would mangle a valid
/// prefixed Hebrew word whenever its English-keystroke reading happened to be an
/// English word — the exact "nested words fixed wrong" bug.
fn decide_known(
    text_en: &str,
    text_he: &str,
    current: Language,
    en_dict: &HashSet<String>,
    he_dict: &HashSet<String>,
) -> Option<Language> {
    let other = current.other();
    let cur_text = if current == Language::English { text_en } else { text_he };
    let oth_text = if other == Language::English { text_en } else { text_he };

    if valid_loose(cur_text, current, en_dict, he_dict) {
        return None;
    }
    if valid_strict(oth_text, other, en_dict, he_dict) {
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
    en_dict: &HashSet<String>,
    he_dict: &HashSet<String>,
) -> Option<Language> {
    let en_strict = valid_strict(text_en, Language::English, en_dict, he_dict);
    let he_strict = valid_strict(text_he, Language::Hebrew, en_dict, he_dict);
    let he_loose = valid_loose(text_he, Language::Hebrew, en_dict, he_dict);
    if en_strict && !he_loose {
        Some(Language::English)
    } else if he_strict && !en_strict {
        Some(Language::Hebrew)
    } else {
        None
    }
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

/// Pure planning step: decide whether (and where) to switch, given the folded
/// buffers, the per-key offset tables, and the live `current` layout. Returns
/// `Some((target_language, start))` where `start` is the key index the acted-on
/// word begins at (`0` = whole buffer). Kept free of any I/O so it is unit
/// testable; the actual layout switch happens in the caller.
fn plan(
    full_en: &str,
    full_he: &str,
    offsets_en: &[usize],
    offsets_he: &[usize],
    keys_len: usize,
    current: Option<Language>,
    en_dict: &HashSet<String>,
    he_dict: &HashSet<String>,
) -> Option<(Language, usize)> {
    // Whole-buffer decision first — this is what fires for virtually every real
    // correction.
    let whole = match current {
        Some(cur) => decide_known(full_en, full_he, cur, en_dict, he_dict),
        None => decide_unknown(full_en, full_he, en_dict, he_dict),
    };
    if let Some(lang) = whole {
        return Some((lang, 0));
    }

    // Missing-space split: opt-in, and only meaningful when we know the layout.
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
            return Some((other, split));
        }
    }

    None
}

/// Run the layout-switch decision over a key sequence.
///
/// Anchors on the live keyboard layout: a sequence that already reads as a real
/// word in the current layout is never touched, and we only switch when the
/// *other* layout yields a confident dictionary word. A missing-space split
/// fallback exists but is opt-in (`RECAST_SPLIT=1`) because it cannot be made
/// reliably safe.
///
/// Returns `Some(start)` when a switch was performed; the word that was acted on
/// begins at `keys[start]` (so `start = 0` means the whole buffer). Callers
/// should delete and retype only `keys[start..]`.
pub fn check_and_switch_with_split<K: Copy>(
    keys: &[K],
    to_en: impl Fn(K) -> Option<char>,
    to_he: impl Fn(K) -> Option<char>,
    en_dict: &HashSet<String>,
    he_dict: &HashSet<String>,
) -> Option<usize> {
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
    let mut full_en = String::with_capacity(keys.len());
    let mut full_he = String::with_capacity(keys.len() * 2);
    let mut offsets_en = Vec::with_capacity(keys.len() + 1);
    let mut offsets_he = Vec::with_capacity(keys.len() + 1);
    offsets_en.push(0);
    offsets_he.push(0);
    for &k in keys {
        if let Some(c) = to_en(k) {
            full_en.push(c);
        }
        if let Some(c) = to_he(k) {
            full_he.push(c);
        }
        offsets_en.push(full_en.len());
        offsets_he.push(full_he.len());
    }

    let current = crate::layout::current_layout();
    let Some((lang, start)) = plan(
        &full_en,
        &full_he,
        &offsets_en,
        &offsets_he,
        keys.len(),
        current,
        en_dict,
        he_dict,
    ) else {
        debug_log(&full_en, &full_he, None, false);
        return None;
    };

    let switched = switch_layout_to(lang);
    if debug_enabled() && start > 0 {
        println!("split @ {}", start);
    }
    debug_log(&full_en, &full_he, Some(lang), switched);
    if switched {
        Some(start)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(words: &[&str]) -> HashSet<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    // English word "gv" -> these are illustrative ASCII stand-ins; the real
    // tests below use actual Hebrew text so the prefix logic is exercised.
    #[test]
    fn real_word_in_current_layout_is_left_alone() {
        let en = dict(&["hello"]);
        let he = dict(&["שלום"]);
        // Typing "hello" while in English layout: do nothing.
        assert_eq!(decide_known("hello", "ימךךם", Language::English, &en, &he), None);
        // Typing "שלום" while in Hebrew layout: do nothing.
        assert_eq!(decide_known("akuo", "שלום", Language::Hebrew, &en, &he), None);
    }

    #[test]
    fn wrong_layout_switches_to_other() {
        let en = dict(&["hello"]);
        let he = dict(&["שלום"]);
        // In Hebrew layout but the keys spell "hello" in English -> switch EN.
        assert_eq!(
            decide_known("hello", "ימךךם", Language::Hebrew, &en, &he),
            Some(Language::English)
        );
        // In English layout but the keys spell "שלום" in Hebrew -> switch HE.
        assert_eq!(
            decide_known("akuo", "שלום", Language::English, &en, &he),
            Some(Language::Hebrew)
        );
    }

    #[test]
    fn prefixed_hebrew_is_not_mangled() {
        // "שלום" is in the dict; "ושלום" (and-peace) is not, but matches via the
        // one-letter prefix. Typed in Hebrew layout it must be left alone.
        let en = dict(&["hello"]);
        let he = dict(&["שלום"]);
        assert_eq!(decide_known("uakuo", "ושלום", Language::Hebrew, &en, &he), None);
    }

    #[test]
    fn prefixed_hebrew_with_english_collision_is_not_mangled() {
        // "ושלום" is a loose-valid prefixed Hebrew word (ו + שלום). Its
        // English-keystroke reading "uakuo" also happens to be an English dict
        // word (a homograph collision). Typed in Hebrew layout it must be left
        // alone — switching here is the "nested words fixed wrong" bug. This
        // only passes because the loose current-layout guard is checked before
        // the strict other-layout trigger.
        let en = dict(&["uakuo"]);
        let he = dict(&["שלום"]);
        assert_eq!(decide_known("uakuo", "ושלום", Language::Hebrew, &en, &he), None);
    }

    #[test]
    fn ambiguous_homograph_trusts_current_layout() {
        // Keys valid as a word in BOTH layouts: never switch, trust current.
        let en = dict(&["go"]);
        let he = dict(&["עט"]); // whatever the keys read as in Hebrew
        assert_eq!(decide_known("go", "עט", Language::English, &en, &he), None);
        assert_eq!(decide_known("go", "עט", Language::Hebrew, &en, &he), None);
    }
}
