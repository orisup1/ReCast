use std::collections::HashSet;
use std::sync::OnceLock;

use crate::layout::switch_layout_to;
use crate::types::Language;

fn debug_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("RECAST_DEBUG").is_some())
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

/// Pure decision: given the same physical key sequence interpreted as English
/// (`word_en`) and Hebrew (`word_he`), return the layout to switch to — or
/// `None` if the word is meaningful in both languages (ambiguous) or in
/// neither.
///
/// Asymmetric validity by role. To flip a word we require two things, and each
/// uses a different strictness:
///
///   * **Means something in the target (other) language** — STRICT direct dict
///     lookup. When the user types an English word missing from `en_dict` (a
///     name, slang, plural, typo, abbreviation), the same keys read as Hebrew
///     often pattern-match "valid Hebrew word + one-letter inflectional prefix
///     ו ה ל ב כ מ ש". Accepting that loose match as the flip *target* fires
///     a wrong switch to Hebrew, so the target side stays strict.
///
///   * **Means nothing in the current (source) language** — LOOSE
///     `matches_hebrew` (prefix-strip) lookup for the Hebrew source. A real
///     Hebrew word the user typed often carries an inflectional prefix and so
///     is absent from `he_dict` directly (only the bare form is stored). With a
///     strict source check those words look meaningless and we'd flip a nested
///     chunk of them to English, mangling the word. Treating them as meaningful
///     Hebrew via `matches_hebrew` blocks the flip — exactly the intent: "to
///     change a word the letters must mean something in the other language AND
///     mean nothing in the current one."
///
/// English has no one-letter inflectional prefixes, so its source check is the
/// same strict `en_dict` lookup either way.
fn decide_target_lang(
    word_en: &str,
    word_he: &str,
    en_dict: &HashSet<String>,
    he_dict: &HashSet<String>,
) -> Option<Language> {
    // Strict membership — used as the "means something in the target language"
    // test (the language we would switch *to*).
    let in_en_strict = !word_en.is_empty() && en_dict.contains(word_en);
    let in_he_strict = !word_he.is_empty() && he_dict.contains(word_he);
    // Loose Hebrew membership (prefix-strip) — used as the "means something in
    // the current language" test for the Hebrew source side.
    let he_meaningful = !word_he.is_empty() && matches_hebrew(word_he, he_dict);
    if in_en_strict && !he_meaningful {
        Some(Language::English)
    } else if in_he_strict && !in_en_strict {
        Some(Language::Hebrew)
    } else {
        None
    }
}

/// True if the key sequence is recognised as a word in either dictionary
/// (used as the prefix-validity check when looking for a missing-space split).
fn is_known_word(
    word_en: &str,
    word_he: &str,
    en_dict: &HashSet<String>,
    he_dict: &HashSet<String>,
) -> bool {
    (!word_en.is_empty() && en_dict.contains(word_en))
        || (!word_he.is_empty() && matches_hebrew(word_he, he_dict))
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

/// Run the layout-switch decision over a key sequence.
///
/// First tries the full buffer (parity with the historical behaviour). If that
/// yields no decision, scans split points from longest prefix down: when the
/// prefix is itself a known word in some dictionary and the suffix decides for
/// a specific layout, treat the suffix as the current word — this catches the
/// case where the user forgot the space between two words (e.g. "hellohello").
///
/// Returns `Some(start)` when a switch was performed; the suffix that was
/// acted on begins at `keys[start]` (so `start = 0` means the whole buffer
/// was used). Callers should delete and retype only `keys[start..]`.
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
    // char lands in the resulting `String`s. Splits then slice into the
    // precomputed buffers instead of re-walking the key vector twice per
    // split point — drops the fallback scan from O(N²) to O(N).
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

    // 1. Full-buffer attempt — preserves the original behaviour exactly.
    if let Some(lang) = decide_target_lang(&full_en, &full_he, en_dict, he_dict) {
        let switched = switch_layout_to(lang);
        debug_log(&full_en, &full_he, Some(lang), switched);
        return if switched { Some(0) } else { None };
    }

    // If the full buffer is itself a valid word in *either* dictionary, do
    // not attempt the split fallback. We only get here when
    // `decide_target_lang` returned `None`, which (since the buffer is
    // non-empty) means the same keystrokes parse as a real word in BOTH
    // languages. Splitting an already-valid ambiguous word is the main
    // source of "I typed a real word and it got replaced anyway" — the
    // split scanner happily finds some sub-interpretation in the other
    // language and triggers a swap. Bail out and trust what the user typed.
    // Loose Hebrew match here so a real Hebrew word carrying an inflectional
    // prefix (absent from `he_dict` directly) still counts as valid and blocks
    // the split scanner from flipping a nested chunk of it.
    let full_en_valid = !full_en.is_empty() && en_dict.contains(&full_en);
    let full_he_valid = !full_he.is_empty() && matches_hebrew(&full_he, he_dict);
    if full_en_valid || full_he_valid {
        debug_log(&full_en, &full_he, None, false);
        return None;
    }

    // 2. Split fallback. Scan split points from longest prefix to shortest;
    //    the first match leaves the most user-typed text intact.
    for split in (1..keys.len()).rev() {
        let prefix_en = &full_en[..offsets_en[split]];
        let prefix_he = &full_he[..offsets_he[split]];
        if !is_known_word(prefix_en, prefix_he, en_dict, he_dict) {
            continue;
        }
        let suffix_en = &full_en[offsets_en[split]..];
        let suffix_he = &full_he[offsets_he[split]..];
        let Some(lang) = decide_target_lang(suffix_en, suffix_he, en_dict, he_dict) else {
            continue;
        };
        let switched = switch_layout_to(lang);
        if debug_enabled() {
            println!(
                "split @ {}: prefix=[{} / {}] suffix=[{} / {}]",
                split, prefix_en, prefix_he, suffix_en, suffix_he,
            );
        }
        debug_log(suffix_en, suffix_he, Some(lang), switched);
        if switched {
            return Some(split);
        }
        // Layout was already correct for this suffix; keep scanning shorter
        // prefixes in case a different split decides differently.
    }

    debug_log(&full_en, &full_he, None, false);
    None
}
