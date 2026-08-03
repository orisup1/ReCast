//! In-language English spelling autocorrect — the second correction pipeline.
//!
//! The layout pipeline in `dictionary.rs` compares the *same keystrokes* read
//! under two layouts. This one stays inside a single language: the keystrokes
//! read as English, but the result is not an English word, so we look for the
//! word the user meant — a near-miss of a common English word.
//!
//! # The model
//!
//! This is a noisy-channel corrector in the shape Kernighan, Church and Gale
//! gave it: the user knew the word `c` they wanted and the channel (their
//! hands, their memory of the spelling) turned it into what we saw, `t`. The
//! correction is
//!
//! ```text
//!     argmax_c  P(c) · P(t | c)
//! ```
//!
//! — a *prior*, how likely anyone was to want that word at all, times a
//! *channel model*, how likely that word was to come out looking like this.
//! Both matter, and the classic failure of edit-distance correctors is to keep
//! only the second: ranking by distance and using frequency merely to break
//! ties makes the channel infinitely more important than the prior, so a
//! marginally cheaper edit into a word nobody types beats an obvious edit into
//! a word everybody does. Here the two are added in log space
//! ([`score`]), so a candidate can win either by being the likelier slip or by
//! being the far likelier word.
//!
//! The channel model is Brill and Moore's: not single characters, but *generic
//! string-to-string edits*, `α → β`, so `ph` → `f` or `ant` → `ent` is one
//! event rather than two or three unrelated ones. That is what lets it reach
//! the misspellings people actually produce — `dependant`, `recieve`, `fone` —
//! which single-character models can only reach by paying full price per
//! letter. Brill and Moore learn the rules and their probabilities from a
//! corpus of misspelling/correction pairs; we have no such corpus, so [`RULES`]
//! is hand-written from the standard English confusions and priced by hand.
//! Their other finding is kept too: edits are conditioned on *where in the word*
//! they happen, because a slip in the opening letters is far rarer than one in
//! the middle (see [`position_penalty`]).
//!
//! # Precision
//!
//! Precision matters far more than recall: a wrong layout switch is at least
//! reversible in the user's head ("I was in the wrong layout"), while a wrong
//! spelling fix silently rewrites a name or an identifier into a different
//! word. So the posterior only ranks candidates that have already cleared a set
//! of hard gates:
//!
//! * the typed word is not itself an English word (the caller guarantees this),
//! * it is not a token the corpus sees often enough to be deliberate — a name
//!   or a handle rather than a typo (see [`correct_with`]),
//! * it is long enough to be a real typo rather than an initialism (`min_len`),
//! * the channel cost is within the budget for a word that length (see
//!   [`budget_for`]) — one edit on a short word, up to three on a long one,
//! * the candidate is a *common* word — inside a rank budget that shrinks as
//!   the edit gets more speculative ([`rank_budget`]) — as well as a dictionary
//!   word.
//!
//! # How candidates are found
//!
//! Rather than generating the ring of all strings one edit away (which explodes
//! past distance 1 — 26² insertions of insertions), we scan the frequency list
//! itself and score each entry. The list is sorted, so each possible opening
//! letter is one contiguous run of it, and the scan is limited to the few
//! openings a correction could plausibly have: the typed word's own first
//! letter, its second (a transposed opening, `hte` → `the`), and whatever a
//! word-initial rule could produce (`fone` → **ph**`one`). A length filter and
//! a letter-bag lower bound then throw out most of what is left before the
//! matrix is touched.
//!
//! Everything here is pure and dictionary-driven, so it is unit testable and
//! never touches the OS.

use std::sync::OnceLock;

use crate::config::Config;
use crate::dictionary::{Dict, Freq};

/// Longest word we try to correct. Beyond this a "word" is almost certainly a
/// URL fragment, a path or an identifier, and scanning for a correction gets
/// pointlessly wide.
const MAX_LEN: usize = 20;

// ─────────────────────────────────────────────────────────────────────────────
// Channel costs, in negative-log-probability units where one ordinary edit — a
// wrong, missing or extra letter with nothing to explain it — is `COST_EDIT`.
// Everything else is priced relative to that by how often people actually do
// it, which is what lets the search rank two candidates the same *number* of
// edits away.
// ─────────────────────────────────────────────────────────────────────────────

/// One plain edit: the unit the distance budget is denominated in.
const COST_EDIT: u32 = 100;
/// Substituting a key for the one **beside it in the same row** — the classic
/// fat finger, and by far the most common wrong-letter typo. The hand is
/// already on that row and the finger lands one key over.
///
/// This is the cheapest substitution there is, and `bag_bound` prunes the whole
/// frequency scan at the cheapest rate any edit offers — so lowering it makes
/// every correction slower (measurably: 65 → 60 costs ~10% of the scan) for a
/// discrimination that the gap to [`COST_ADJACENT_DIAG`] already provides.
const COST_ADJACENT_ROW: u32 = 65;
/// The same slip onto the row above or below (`g` for `t`, `g` for `b`). Still
/// a finger in the wrong place, but reaching across rows is a deliberate
/// movement that goes wrong less often than sliding along one.
const COST_ADJACENT_DIAG: u32 = 72;
/// An **extra** letter next to one of its neighbours on the keyboard: the hand
/// caught a second key on its way past (`worjk` for `work`, `mnake` for
/// `make`). Distinct from a stray letter the writer believed in — that is a
/// misspelling and still costs a full edit — and dearer than a fat-fingered
/// substitution, since hitting two keys is less common than hitting the wrong
/// one.
const COST_STRAY_KEY: u32 = 70;
/// Two letters typed in the wrong order.
const COST_TRANSPOSE: u32 = 60;
/// Half of a double letter dropped (`hello` → `helo`) or a single letter typed
/// twice (`help` → `hellp`). The most frequent real-world typo there is.
const COST_DOUBLE: u32 = 55;
/// One vowel for another (`seperate` → `separate`). Not a finger slip but a
/// spelling doubt, and still far likelier than an arbitrary letter swap.
const COST_VOWEL: u32 = 85;

/// Surcharge on an edit that lands on the word's **first** letter. Brill and
/// Moore condition every edit on its position in the word for exactly this
/// reason: people get the opening of a word right far more reliably than the
/// middle, and rewriting it is the most damaging kind of wrong correction —
/// it is what turns a name into an unrelated word. Priced so that a first-letter
/// substitution alone (100 + 90) needs a two-edit budget to be affordable.
const COST_INITIAL: u32 = 90;
/// The same idea, much weaker, for the second letter.
const COST_SECOND: u32 = 25;

/// Shortest word allowed a two-edit correction. Below this, two edits change so
/// much of the word that the "correction" is usually a different word — and
/// short unknown tokens are overwhelmingly names, which is exactly what must
/// not be rewritten.
const DIST2_MIN_LEN: usize = 7;
/// Shortest word allowed a three-edit correction. A word this long that is
/// three edits from a very common word is almost always that word, badly typed.
const DIST3_MIN_LEN: usize = 10;

/// A two-edit suggestion has to be this many times more common than a one-edit
/// one would need to be: two edits is a much weaker signal, so only genuinely
/// frequent words are allowed to win that way.
const DIST2_RANK_FACTOR: u32 = 2;
/// Same for three edits, harder still.
const DIST3_RANK_FACTOR: u32 = 5;

// ─────────────────────────────────────────────────────────────────────────────
// The prior, P(c).
// ─────────────────────────────────────────────────────────────────────────────

/// Weight of the prior against the channel. Word frequencies are Zipfian, so
/// the negative log probability of a word is essentially the log of its rank,
/// and this is the exchange rate between "log ranks" and "channel cost units".
///
/// At 8, a word ten times more common than another is worth ~18 cost units —
/// less than the gap between a finger slip and a plain edit. That is the
/// intended balance, and the calibration tests against the real corpus are what
/// set it: any higher and `fomr` starts resolving to the far more common `for`
/// rather than the obvious transposition of `form`.
const PRIOR_WEIGHT: f32 = 8.0;

/// Added to a rank before taking its log, so rank 0 is finite and the very top
/// of the list isn't spread out absurdly far from the rest of it.
const PRIOR_SMOOTHING: f32 = 10.0;

/// Posterior score of a candidate: the channel cost of the slip plus the
/// improbability of the word, both as negative log probabilities, so lower is
/// better and `argmax P(c)·P(t|c)` becomes a plain sum.
fn score(cost: u32, rank: u32) -> f32 {
    cost as f32 + PRIOR_WEIGHT * (rank as f32 + PRIOR_SMOOTHING).ln()
}

// ─────────────────────────────────────────────────────────────────────────────
// The Brill–Moore rule table.
// ─────────────────────────────────────────────────────────────────────────────

/// A generic string-to-string edit: the user typed `from` where `to` was meant,
/// and that whole exchange costs `cost` — one event, not one per letter.
struct Rule {
    /// What appears in the typed word.
    from: &'static str,
    /// What appears in the intended word.
    to: &'static str,
    cost: u32,
}

const fn rule(from: &'static str, to: &'static str, cost: u32) -> Rule {
    Rule { from, to, cost }
}

/// Longest side any rule may have. The matrix look-back is bounded by this, and
/// so is the letter-bag lower bound that prunes the scan.
const MAX_RULE_LEN: usize = 4;

/// How many rows back an alignment can reach: `MAX_RULE_LEN` for a block edit,
/// two for a transposition. The matrix's early bail-out is only sound over a
/// window this deep.
const LOOKBACK: usize = MAX_RULE_LEN;

/// Cost of a systematic spelling confusion — a rule the writer applied
/// consistently because they believe that is how the word is spelled. Cheaper
/// than a plain edit (it is one decision, not several slips) but dearer than a
/// finger slip, because it rewrites more of the word.
const COST_SPELLING: u32 = 70;
/// Cost of the strongest confusions — the ones that are pure orthography, where
/// the two spellings sound identical and the writer had no phonetic cue at all
/// (`ph`/`f`, `kn`/`n`, silent letters).
const COST_HOMOPHONE: u32 = 55;

/// String-to-string edits, `typed` → `intended`.
///
/// Brill and Moore derive this table and its probabilities from a corpus of
/// misspelling/correction pairs. We have no such corpus, so these are the
/// standard English confusions written out by hand and priced by how systematic
/// each one is. Both directions are listed where both directions happen.
///
/// Two constraints the code depends on: neither side may exceed
/// [`MAX_RULE_LEN`], and `from` must not be empty (the matrix looks the rule up
/// by the typed letter it ends on). A third is a matter of judgement: a rule
/// that moves many letters at once for a single cost drags down the rate the
/// scan's letter-bag pruning is derived from, blunting it for every other word.
/// That is why wholesale phonetic respellings (`shun` → `tion`) are not here:
/// they are past what this corrector is for, and they are not free.
static RULES: &[Rule] = &[
    // Silent and phonetic consonant clusters. These are what make a phonetic
    // spelling reachable at all: "fone" and "phone" differ by two letters and
    // one sound.
    rule("f", "ph", COST_HOMOPHONE),
    rule("ph", "f", COST_HOMOPHONE),
    rule("f", "gh", COST_HOMOPHONE),
    rule("gh", "f", COST_HOMOPHONE),
    rule("n", "kn", COST_HOMOPHONE),
    rule("kn", "n", COST_HOMOPHONE),
    rule("n", "gn", COST_HOMOPHONE),
    rule("r", "wr", COST_HOMOPHONE),
    rule("wr", "r", COST_HOMOPHONE),
    rule("m", "mb", COST_HOMOPHONE),
    rule("w", "wh", COST_HOMOPHONE),
    rule("wh", "w", COST_HOMOPHONE),
    rule("k", "ck", COST_HOMOPHONE),
    rule("ck", "k", COST_HOMOPHONE),
    rule("k", "c", COST_HOMOPHONE),
    rule("c", "k", COST_HOMOPHONE),
    rule("s", "c", COST_HOMOPHONE),
    rule("c", "s", COST_HOMOPHONE),
    rule("s", "z", COST_HOMOPHONE),
    rule("z", "s", COST_HOMOPHONE),
    rule("x", "ks", COST_HOMOPHONE),
    rule("ks", "x", COST_HOMOPHONE),
    rule("j", "g", COST_SPELLING),
    rule("g", "j", COST_SPELLING),
    // Vowel digraphs — the sound is one, the spelling is a coin flip.
    rule("ie", "ei", COST_SPELLING),
    rule("ei", "ie", COST_SPELLING),
    rule("ee", "ea", COST_SPELLING),
    rule("ea", "ee", COST_SPELLING),
    rule("ee", "ie", COST_SPELLING),
    rule("ie", "ee", COST_SPELLING),
    rule("oo", "u", COST_SPELLING),
    rule("u", "oo", COST_SPELLING),
    rule("o", "ou", COST_SPELLING),
    rule("ou", "o", COST_SPELLING),
    rule("i", "y", COST_SPELLING),
    rule("y", "i", COST_SPELLING),
    // Suffixes people genuinely do not know the spelling of. These are the
    // rules that pay for themselves: "dependant", "existance", "seperatly" are
    // one decision away from right, not two or three slips.
    rule("ant", "ent", COST_SPELLING),
    rule("ent", "ant", COST_SPELLING),
    rule("ance", "ence", COST_SPELLING),
    rule("ence", "ance", COST_SPELLING),
    rule("ancy", "ency", COST_SPELLING),
    rule("ency", "ancy", COST_SPELLING),
    rule("able", "ible", COST_SPELLING),
    rule("ible", "able", COST_SPELLING),
    rule("cion", "tion", COST_SPELLING),
    rule("sion", "tion", COST_SPELLING),
    rule("tion", "sion", COST_SPELLING),
    rule("us", "ous", COST_SPELLING),
    rule("ous", "us", COST_SPELLING),
    rule("aly", "ally", COST_SPELLING),
    rule("ly", "lly", COST_SPELLING),
    rule("cal", "cle", COST_SPELLING),
    rule("cle", "cal", COST_SPELLING),
    rule("er", "re", COST_SPELLING),
    rule("re", "er", COST_SPELLING),
    rule("ar", "er", COST_SPELLING),
    rule("er", "ar", COST_SPELLING),
    rule("or", "er", COST_SPELLING),
    rule("er", "or", COST_SPELLING),
    rule("ur", "er", COST_SPELLING),
];

/// The rules that could apply at a given typed cell, indexed by the letter the
/// typed side ends on.
///
/// Without this the matrix would test all ~60 rules in every cell, which costs
/// more than the rest of the scan put together. With it, a cell looks at the
/// two or three rules that could possibly match there.
fn rules_by_last_byte() -> &'static [Vec<&'static Rule>; 26] {
    static INDEX: OnceLock<[Vec<&'static Rule>; 26]> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index: [Vec<&'static Rule>; 26] = std::array::from_fn(|_| Vec::new());
        for rule in RULES {
            let last = *rule.from.as_bytes().last().expect("a rule needs a typed side");
            index[(last - b'a') as usize].push(rule);
        }
        index
    })
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
///
/// `max_dist` is an upper bound in whole edits (0 disables correction entirely);
/// the word's own length can lower it further — see [`budget_for`].
pub fn correct_with(
    word: &str,
    en_dict: Dict,
    en_freq: Freq,
    min_len: usize,
    max_rank: u32,
    max_dist: u8,
) -> Option<String> {
    let budget = budget_for(word, min_len, max_dist)?;
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

    let typed = word.as_bytes();
    let typed_letters = letter_counts(typed);
    // (score, cost, rank, word) — the score decides, the rest only makes ties
    // deterministic.
    let mut best: Option<(f32, u32, u32, String)> = None;
    let mut dp = Dp::default();

    for opening in openings(word) {
        let prefix = [opening.letter];
        let Ok(prefix) = std::str::from_utf8(&prefix) else {
            continue;
        };
        en_freq.for_each_with_prefix(prefix, |cand, rank| {
            // Cheap gates first: the whole point of scanning the list is that
            // almost every entry is thrown out before the matrix is touched.
            let cb = cand.as_bytes();
            if rank > max_rank || !opening.admits(typed, cb) {
                return;
            }
            let len_gap = cb.len().abs_diff(typed.len()) as u32 * COST_EDIT;
            if len_gap > budget || bag_bound(&typed_letters, cb) > budget {
                return;
            }
            let Some(cost) = dp.distance(typed, cb, budget) else {
                return;
            };
            if cost == 0 || rank > rank_budget(cost, max_rank) {
                return;
            }
            let candidate = score(cost, rank);
            let better = best.as_ref().is_none_or(|(cur, cur_cost, cur_rank, _)| {
                (candidate, cost, rank) < (*cur, *cur_cost, *cur_rank)
            });
            // Checked last: it is the only expensive test, and a candidate that
            // isn't going to win doesn't need it.
            if better && en_dict.contains(cand) {
                best = Some((candidate, cost, rank, cand.to_string()));
            }
        });
    }

    best.map(|(_, _, _, fixed)| fixed)
}

/// One run of the frequency list worth scanning, and the reason it is worth
/// scanning — which is also the only thing a candidate found there is allowed
/// to be.
#[derive(Clone, Copy)]
struct Opening {
    /// First letter of the run.
    letter: u8,
    why: Why,
}

#[derive(Clone, Copy, PartialEq)]
enum Why {
    /// The word's own first letter: anything in this run is worth scoring.
    Kept,
    /// The word's *second* letter. Nothing lands here except a transposed
    /// opening, so nothing else is scored — which is what stops a second full
    /// run of the list from costing anything.
    Transposed,
    /// A word-initial rule, e.g. `f` → `ph`: the candidate has to actually
    /// begin with what the rule intended.
    Rule(&'static str),
}

impl Opening {
    /// Whether `cand` is the kind of word this run was opened for.
    fn admits(self, typed: &[u8], cand: &[u8]) -> bool {
        match self.why {
            Why::Kept => true,
            Why::Transposed => {
                cand.len() >= 2 && typed.len() >= 2 && cand[0] == typed[1] && cand[1] == typed[0]
            }
            Why::Rule(to) => cand.starts_with(to.as_bytes()),
        }
    }
}

/// The first letters a correction of `word` could plausibly have.
///
/// This is the one place recall is traded for speed, and it is deliberate: the
/// opening of a word is what people get right, so rather than scan all 26
/// letters we scan the ones an edit at position 0 could actually produce — the
/// typed letter itself, the second letter for a transposed opening (`hte` →
/// `the`), and the first letter of any rule matching at the start of the word,
/// which is what makes `fone` → `phone` reachable. A plain wrong first letter
/// stays out of reach by construction, and it is also the correction we would
/// least want to make.
fn openings(word: &str) -> Vec<Opening> {
    let typed = word.as_bytes();
    let mut openings = vec![Opening {
        letter: typed[0],
        why: Why::Kept,
    }];
    if typed.len() >= 2 && typed[1] != typed[0] {
        openings.push(Opening {
            letter: typed[1],
            why: Why::Transposed,
        });
    }
    for rule in RULES {
        if !word.starts_with(rule.from) {
            continue;
        }
        let Some(&letter) = rule.to.as_bytes().first() else {
            continue;
        };
        if letter == typed[0] {
            continue; // already covered by the run we are scanning anyway
        }
        openings.push(Opening {
            letter,
            why: Why::Rule(rule.to),
        });
    }
    openings
}

/// The channel-cost budget for `word`, or `None` if the word is not the kind of
/// token we are willing to rewrite at all: plain lowercase ASCII letters (no
/// digits — `a4` is an identifier, not a typo) and long enough that a one-letter
/// edit doesn't turn one word into an unrelated one.
///
/// The budget grows with length because the risk does not: a three-edit
/// neighbour of a four-letter word is a different word, while a three-edit
/// neighbour of an eleven-letter word is that word with a bad day.
fn budget_for(word: &str, min_len: usize, max_dist: u8) -> Option<u32> {
    if !eligible(word, min_len) {
        return None;
    }
    let by_length = if word.len() >= DIST3_MIN_LEN {
        3
    } else if word.len() >= DIST2_MIN_LEN {
        2
    } else {
        1
    };
    let edits = by_length.min(max_dist as u32);
    (edits > 0).then(|| edits * COST_EDIT)
}

/// Whether `word` is a token the speller may touch at all.
fn eligible(word: &str, min_len: usize) -> bool {
    word.len() >= min_len
        && word.len() <= MAX_LEN
        && word.bytes().all(|b| b.is_ascii_lowercase())
}

/// Worst frequency rank a suggestion of this channel cost may have. A candidate
/// that is only a slip away can be a fairly ordinary word; one that is three
/// edits away has to be one of the most common words in the language before we
/// believe it. This is a hard gate, separate from the prior in [`score`]: the
/// prior ranks what survives, this decides what is allowed to survive.
fn rank_budget(cost: u32, max_rank: u32) -> u32 {
    if cost <= COST_EDIT {
        max_rank
    } else if cost <= 2 * COST_EDIT {
        max_rank / DIST2_RANK_FACTOR
    } else {
        max_rank / DIST3_RANK_FACTOR
    }
}

/// Surcharge for an edit landing on typed position `i` — Brill and Moore's
/// conditioning on where in the word the slip happened, and what replaces the
/// old hard "never change the first letter" rule. A first-letter edit is not
/// forbidden now, merely expensive enough that it needs real evidence (a long
/// word, a cheap remainder, a common target) to pay for itself.
///
/// Rules are exempt — `ph` → `f` at the start of a word is not a slip, it is how
/// the writer thinks the word is spelled — and so are transpositions, which keep
/// every letter and only reorder them (`hte` → `the`).
fn position_penalty(i: usize) -> u32 {
    match i {
        0 => COST_INITIAL,
        1 => COST_SECOND,
        _ => 0,
    }
}

/// How many of each letter a word contains.
fn letter_counts(word: &[u8]) -> [i16; 26] {
    let mut counts = [0i16; 26];
    for &c in word {
        if c.is_ascii_lowercase() {
            counts[(c - b'a') as usize] += 1;
        }
    }
    counts
}

/// The cheapest any edit can be, per letter of bag difference it can account
/// for, in hundredths of a cost unit. Derived from the tables rather than
/// written down, so adding a cheap or wide rule can't silently invalidate the
/// pruning in [`bag_bound`].
fn min_cost_per_letter_x100() -> u32 {
    static MIN: OnceLock<u32> = OnceLock::new();
    *MIN.get_or_init(|| {
        // A substitution changes two letter counts, an insertion or deletion
        // one, a transposition none (so it can never help close a bag gap).
        // Every cheap edit has to be represented here or the bound stops being
        // one: a discount the matrix can give but this cannot see would let
        // `bag_bound` reject a candidate the matrix would have accepted.
        let cheapest_gap = COST_DOUBLE.min(COST_STRAY_KEY);
        let cheapest_sub = COST_ADJACENT_ROW.min(COST_ADJACENT_DIAG).min(COST_VOWEL);
        let mut min = (cheapest_gap * 100).min(cheapest_sub * 100 / 2);
        for rule in RULES {
            // What the rule actually moves, not how long it is: `ance` → `ence`
            // is four letters wide but only exchanges one of them.
            let moved = bag_difference(rule.from.as_bytes(), rule.to.as_bytes());
            if moved > 0 {
                min = min.min(rule.cost * 100 / moved);
            }
        }
        min
    })
}

/// Total letter-count difference between two words — how far apart their bags
/// of letters are, ignoring order entirely.
fn bag_difference(a: &[u8], b: &[u8]) -> u32 {
    let mut diff = letter_counts(a);
    for &c in b {
        if c.is_ascii_lowercase() {
            diff[(c - b'a') as usize] -= 1;
        }
    }
    diff.iter().map(|d| d.unsigned_abs() as u32).sum()
}

/// A lower bound on the channel cost of turning the typed word into `cand`,
/// from their letter counts alone — no alignment, no matrix.
///
/// No edit can close more letter-count difference than it is worth: a
/// substitution moves the two bags two steps closer, an insertion or deletion
/// one, a rule at most as many as it has letters, and a transposition none. So
/// the total count difference priced at the cheapest rate any edit offers can
/// never overstate the real distance — and it throws out most of the frequency
/// list for a few additions per candidate, which is what keeps a scan under a
/// millisecond.
fn bag_bound(typed: &[i16; 26], cand: &[u8]) -> u32 {
    let mut diff = *typed;
    for &c in cand {
        if c.is_ascii_lowercase() {
            diff[(c - b'a') as usize] -= 1;
        }
    }
    let total: u32 = diff.iter().map(|d| d.unsigned_abs() as u32).sum();
    total * min_cost_per_letter_x100() / 100
}

fn is_vowel(c: u8) -> bool {
    matches!(c, b'a' | b'e' | b'i' | b'o' | b'u' | b'y')
}

/// Position of `c` on a QWERTY keyboard as `(row, 2×column)`. The doubled
/// column lets the half-key stagger between rows be expressed in integers.
fn qwerty_pos(c: u8) -> Option<(i32, i32)> {
    const ROWS: [&[u8]; 3] = [b"qwertyuiop", b"asdfghjkl", b"zxcvbnm"];
    for (row, keys) in ROWS.iter().enumerate() {
        if let Some(col) = keys.iter().position(|&k| k == c) {
            // Each row sits half a key to the right of the one above it.
            return Some((row as i32, col as i32 * 2 + row as i32));
        }
    }
    None
}

/// Neighbours of each letter as bitmasks over `a..=z`, built once from
/// [`qwerty_pos`]: `.0` is the key beside it in the same row, `.1` the keys on
/// the row above or below. Two masks rather than one because the two slips are
/// not equally likely — see [`COST_ADJACENT_ROW`] and [`COST_ADJACENT_DIAG`].
///
/// The matrix asks about adjacency in every substitution cell, and walking the
/// keyboard rows to answer costs more than the rest of the cell put together —
/// this is the single hottest lookup in the scan.
fn adjacency_table() -> &'static [(u32, u32); 26] {
    static TABLE: OnceLock<[(u32, u32); 26]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [(0u32, 0u32); 26];
        for a in 0..26u8 {
            for b in 0..26u8 {
                let (x, y) = (b'a' + a, b'a' + b);
                if let (Some((r1, c1)), Some((r2, c2))) = (qwerty_pos(x), qwerty_pos(y)) {
                    if (r1 - r2).abs() > 1 || (c1 - c2).abs() > 2 || (r1, c1) == (r2, c2) {
                        continue;
                    }
                    let entry = &mut table[a as usize];
                    if r1 == r2 {
                        entry.0 |= 1 << b;
                    } else {
                        entry.1 |= 1 << b;
                    }
                }
            }
        }
        table
    })
}

/// What typing `b` instead of `a` would cost as a finger slip, or `None` if the
/// two keys are nowhere near each other and one cannot explain the other.
fn adjacent_cost(a: u8, b: u8) -> Option<u32> {
    if !a.is_ascii_lowercase() || !b.is_ascii_lowercase() {
        return None;
    }
    let (row, diag) = adjacency_table()[(a - b'a') as usize];
    let bit = 1 << (b - b'a');
    if row & bit != 0 {
        Some(COST_ADJACENT_ROW)
    } else if diag & bit != 0 {
        Some(COST_ADJACENT_DIAG)
    } else {
        None
    }
}

/// Whether two letters are neighbouring keys — the substitution a hurrying hand
/// actually makes.
fn adjacent(a: u8, b: u8) -> bool {
    adjacent_cost(a, b).is_some()
}

/// Every substitution cost, precomputed for all 676 letter pairs.
///
/// The substitution cell is the hottest arithmetic in the program — every cell
/// of every matrix of every candidate in the scan asks for one — and answering
/// it by testing masks and vowels costs more than the table lookup that
/// replaces it. Fits in `u8` because [`COST_EDIT`] is the most anything costs.
fn sub_table() -> &'static [[u8; 26]; 26] {
    static TABLE: OnceLock<[[u8; 26]; 26]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [[0u8; 26]; 26];
        for a in 0..26u8 {
            for b in 0..26u8 {
                let (x, y) = (b'a' + a, b'a' + b);
                table[a as usize][b as usize] = if x == y {
                    0
                } else if let Some(slip) = adjacent_cost(x, y) {
                    slip as u8
                } else if is_vowel(x) && is_vowel(y) {
                    COST_VOWEL as u8
                } else {
                    COST_EDIT as u8
                };
            }
        }
        table
    })
}

/// Cost of typing `y` where `x` was meant.
fn sub_cost(x: u8, y: u8) -> u32 {
    if !x.is_ascii_lowercase() || !y.is_ascii_lowercase() {
        return if x == y { 0 } else { COST_EDIT };
    }
    sub_table()[(x - b'a') as usize][(y - b'a') as usize] as u32
}

/// Cost of the intended word having a letter at `w[i - 1]` that never got
/// typed. Dropping one half of a double letter is a slip of timing; dropping a
/// letter that stands alone is a misspelling.
///
/// Deliberately says nothing about the keyboard: adjacency explains keys that
/// were *hit*, and there is no sense in which a letter is missing because of
/// where its key sits.
fn missing_cost(w: &[u8], i: usize) -> u32 {
    let c = w[i - 1];
    let doubled = (i >= 2 && w[i - 2] == c) || (i < w.len() && w[i] == c);
    if doubled {
        COST_DOUBLE
    } else {
        COST_EDIT
    }
}

/// Cost of the typed word carrying an extra `w[i - 1]` the intended word does
/// not have — and the other half of the asymmetry with [`missing_cost`].
///
/// An extra letter has two innocent explanations and one guilty one. It can be
/// half of a double typed twice; it can be a neighbouring key the hand caught
/// on the way past, which is what the keyboard geometry is for; or the writer
/// put a letter there because they thought it belonged, which is a misspelling
/// and pays the full edit. Telling the three apart is most of the value of
/// knowing where the keys are: `mnake` and `amke` are both one edit from
/// `make` by letter count, but only one of them is a hand missing.
fn extra_cost(w: &[u8], i: usize) -> u32 {
    let c = w[i - 1];
    let before = (i >= 2).then(|| w[i - 2]);
    let after = w.get(i).copied();
    if before == Some(c) || after == Some(c) {
        COST_DOUBLE
    } else if [before, after].into_iter().flatten().any(|n| adjacent(n, c)) {
        COST_STRAY_KEY
    } else {
        COST_EDIT
    }
}

/// Scratch matrix, reused across candidates so a scan of the frequency list
/// allocates a handful of times rather than thousands.
#[derive(Default)]
struct Dp {
    cells: Vec<u32>,
    /// `extra_cost` for each position of the typed word, computed once per
    /// candidate instead of once per cell. It depends only on the typed word,
    /// which is the same for every one of the thousands of candidates in a
    /// scan — and the deletion cell is on the hot path, where two adjacency
    /// lookups per cell were costing ~10% of the whole correction.
    extra: Vec<u32>,
}

impl Dp {
    /// Channel cost of the typed word `a` having come out of the intended word
    /// `b` — a weighted Damerau-Levenshtein alignment extended with the
    /// [`RULES`] block edits and [`position_penalty`] — or `None` once every
    /// alignment in flight has already exceeded `budget`.
    ///
    /// The early bail is what makes scanning the frequency list cheap: for most
    /// entries the whole row goes over budget within a couple of letters and the
    /// rest of the matrix is never computed.
    fn distance(&mut self, a: &[u8], b: &[u8], budget: u32) -> Option<u32> {
        let (n, m) = (a.len(), b.len());
        let width = m + 1;
        // Rules look back up to MAX_RULE_LEN rows and columns, so the whole
        // matrix has to stay addressable — no rolling rows here. Only grown,
        // never cleared: every cell is written before anything reads it, and
        // blanking ~200 cells per candidate costs more than the scan it serves.
        if self.cells.len() < width * (n + 1) {
            self.cells.resize(width * (n + 1), 0);
        }
        let at = |i: usize, j: usize| i * width + j;

        self.extra.clear();
        self.extra.push(0); // unused: positions are 1-based here
        self.extra.extend((1..=n).map(|i| extra_cost(a, i)));

        self.cells[0] = 0;
        for j in 1..=m {
            // Turning the empty prefix of `a` into `b[..j]` is all insertions,
            // every one of them at the start of the typed word.
            self.cells[j] = self.cells[j - 1]
                .saturating_add(missing_cost(b, j))
                .saturating_add(position_penalty(0));
        }

        let index = rules_by_last_byte();
        // Minima of the last few rows. Bailing on a single over-budget row
        // would be wrong: a transposition reaches back two rows and a rule up
        // to `MAX_RULE_LEN`, so a row that looks hopeless can still be jumped
        // over — which is exactly what happens to a transposed opening, whose
        // first rows carry the position penalty the transposition itself is
        // exempt from. Only when *every* row still in reach is over budget can
        // nothing below it come in under.
        let mut recent = [0u32; LOOKBACK];
        for i in 1..=n {
            let mut row_min = u32::MAX;
            for j in 0..=m {
                let mut best = if j == 0 {
                    // All deletions: the typed word has a prefix the intended
                    // word doesn't.
                    self.cells[at(i - 1, 0)]
                        .saturating_add(self.extra[i])
                        .saturating_add(position_penalty(i - 1))
                } else {
                    // `a` is what was typed and `b` what was meant, so the two
                    // directions are not the same event: a letter only in `b`
                    // was dropped, one only in `a` was added — and only the
                    // second can be explained by where the keys are.
                    let insertion = self.cells[at(i, j - 1)]
                        .saturating_add(missing_cost(b, j))
                        .saturating_add(position_penalty(i));
                    let deletion = self.cells[at(i - 1, j)]
                        .saturating_add(self.extra[i])
                        .saturating_add(position_penalty(i - 1));
                    let substitution = self.cells[at(i - 1, j - 1)]
                        .saturating_add(sub_cost(a[i - 1], b[j - 1]))
                        .saturating_add(if a[i - 1] == b[j - 1] {
                            0
                        } else {
                            position_penalty(i - 1)
                        });
                    let mut best = insertion.min(deletion).min(substitution);
                    // A transposition keeps every letter and only reorders
                    // them, so it carries no position penalty — which is what
                    // keeps "hte" → "the" cheap.
                    if i >= 2 && j >= 2 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                        best = best.min(
                            self.cells[at(i - 2, j - 2)].saturating_add(COST_TRANSPOSE),
                        );
                    }
                    best
                };

                // Brill–Moore block edits: one event covering several letters
                // on each side. Indexed by the typed letter this cell ends on,
                // so only a couple of rules are ever tested here.
                for rule in &index[(a[i - 1] - b'a') as usize] {
                    let (fl, tl) = (rule.from.len(), rule.to.len());
                    if i < fl || j < tl {
                        continue;
                    }
                    if &a[i - fl..i] == rule.from.as_bytes()
                        && &b[j - tl..j] == rule.to.as_bytes()
                    {
                        best = best.min(self.cells[at(i - fl, j - tl)].saturating_add(rule.cost));
                    }
                }

                self.cells[at(i, j)] = best;
                row_min = row_min.min(best);
            }
            recent[i % LOOKBACK] = row_min;
            if recent.iter().all(|&min| min > budget) {
                return None;
            }
        }

        let total = self.cells[at(n, m)];
        (total <= budget).then_some(total)
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

    /// Defaults matching `Config::from_env`, so the tests exercise shipped
    /// behaviour rather than a hand-tuned configuration.
    fn fix(word: &str, d: Dict, f: Freq) -> Option<String> {
        correct_with(
            word,
            d,
            f,
            crate::config::DEFAULT_SPELL_MIN_LEN,
            crate::config::DEFAULT_SPELL_MAX_RANK,
            crate::config::DEFAULT_SPELL_MAX_DIST,
        )
    }

    #[test]
    fn corrects_a_one_letter_typo() {
        let d = dict(&["hello", "hell"]);
        let f = freq(&[("hello", 500), ("hell", 4000)]);
        // "helo" is one insertion away from "hello" and one substitution away
        // from "hell"; restoring the dropped half of the "ll" is the cheaper
        // edit, and the more common word too.
        assert_eq!(fix("helo", d, f).as_deref(), Some("hello"));
    }

    #[test]
    fn a_slip_beats_a_more_common_misspelling() {
        let d = dict(&["hello", "help"]);
        // "help" is the more common word, but restoring the dropped half of the
        // "ll" is likelier enough to outweigh the small frequency gap.
        let f = freq(&[("help", 140), ("hello", 203)]);
        assert_eq!(fix("helo", d, f).as_deref(), Some("hello"));
        // Same the other way round: an extra letter in a double.
        assert_eq!(fix("hellp", d, f).as_deref(), Some("help"));
    }

    #[test]
    fn a_far_more_common_word_outweighs_a_slightly_likelier_slip() {
        // The noisy channel's whole point: the prior is a factor, not a
        // tie-break. "hellp" is a cheaper edit from "hello" (un-doubling an "l")
        // than from "help" (an adjacent-key insertion), but when "help" is four
        // orders of magnitude more common, "help" is what the user meant.
        // "helo" is a cheaper edit from "hello" (restoring a double) than from
        // "help" (an adjacent-key substitution) …
        let d = dict(&["hello", "help"]);
        let f = freq(&[("help", 5), ("hello", 40_000)]);
        // … but four orders of magnitude of frequency outweigh that.
        assert_eq!(fix("helo", d, f).as_deref(), Some("help"));
        // Ranked close together, the likelier slip wins again.
        let f_close = freq(&[("help", 5), ("hello", 12)]);
        assert_eq!(fix("helo", d, f_close).as_deref(), Some("hello"));
    }

    #[test]
    fn a_neighbouring_key_beats_a_distant_one() {
        // "s" and "d" are neighbours; "s" and "t" are not. Both candidates are
        // one substitution away and the distant one is the more common word.
        let d = dict(&["wand", "want"]);
        let f = freq(&[("want", 100), ("wand", 900)]);
        assert_eq!(fix("wans", d, f).as_deref(), Some("wand"));
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
    fn a_wrong_first_letter_is_out_of_reach() {
        // "sork" is one substitution from both "work" and "sort". Only the one
        // that keeps the first letter is even scanned for, and that is
        // deliberate: rewriting the opening of a word is what turns a name into
        // an unrelated word.
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
    fn a_short_word_never_gets_a_two_edit_correction() {
        // "cloud" is two edits from "claude" — a name, and short enough that the
        // length gate refuses the second edit no matter how common the
        // candidate is.
        let d = dict(&["cloud"]);
        let f = freq(&[("cloud", 900)]);
        assert_eq!(fix("claude", d, f), None);
        assert_eq!(correct_with("claude", d, f, 4, 20_000, 3), None);
    }

    #[test]
    fn a_long_word_gets_a_two_edit_correction() {
        let d = dict(&["keyboard"]);
        let f = freq(&[("keyboard", 3_000)]);
        // Two edits (missing "y", swapped "ao") on an eight-letter word.
        assert_eq!(fix("keboadr", d, f).as_deref(), Some("keyboard"));
        // Past one edit the rank budget is halved, so 3000 passes under
        // 20000/2 = 10000 …
        assert_eq!(
            correct_with("keboadr", d, f, 4, 20_000, 2).as_deref(),
            Some("keyboard")
        );
        // … but the same word would not clear a tighter budget.
        assert_eq!(correct_with("keboadr", d, f, 4, 4_000, 2), None);
        // And capping the distance at one edit rules it out entirely.
        assert_eq!(correct_with("keboadr", d, f, 4, 20_000, 1), None);
    }

    #[test]
    fn a_badly_mangled_long_word_is_still_recovered() {
        // Two slips in one word — a dropped "a" and a doubled "l" — which a
        // one-edit speller has to give up on and a nine-letter word can afford.
        let d = dict(&["beautiful"]);
        let f = freq(&[("beautiful", 800)]);
        assert_eq!(fix("beutifull", d, f).as_deref(), Some("beautiful"));
        assert_eq!(correct_with("beutifull", d, f, 4, 20_000, 1), None);
        // Past one edit the candidate has to be twice as common: 800 clears
        // 20000/2, but not 1000/2 = 500.
        assert_eq!(correct_with("beutifull", d, f, 4, 1_000, 3), None);
    }

    #[test]
    fn one_edit_wins_over_two() {
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
    fn distance_zero_disables_correction() {
        let d = dict(&["hello"]);
        let f = freq(&[("hello", 500)]);
        assert_eq!(correct_with("helo", d, f, 4, 20_000, 0), None);
    }

    #[test]
    fn a_spelling_rule_costs_one_edit_not_several() {
        // The Brill–Moore payoff: "ant" → "ent" is one decision the writer made,
        // so "dependant" is a single (cheap) edit from "dependent" rather than
        // the two or three a single-character model would charge.
        let mut dp = Dp::default();
        let cost = |dp: &mut Dp, a: &str, b: &str| {
            dp.distance(a.as_bytes(), b.as_bytes(), 10 * COST_EDIT)
        };
        assert_eq!(cost(&mut dp, "dependant", "dependent"), Some(COST_SPELLING));
        assert_eq!(cost(&mut dp, "existance", "existence"), Some(COST_SPELLING));
        // Word-initial rules carry no position penalty — that is the whole
        // reason a phonetic spelling is reachable at all.
        assert_eq!(cost(&mut dp, "fone", "phone"), Some(COST_HOMOPHONE));
        assert_eq!(cost(&mut dp, "nife", "knife"), Some(COST_HOMOPHONE));
        // And a rule beats spelling the same change out letter by letter.
        assert!(cost(&mut dp, "fisical", "physical").unwrap() < 2 * COST_EDIT);
    }

    #[test]
    fn a_first_letter_edit_is_expensive_but_not_forbidden() {
        let mut dp = Dp::default();
        // Substituting the first letter costs the edit plus the surcharge …
        assert_eq!(
            dp.distance(b"sort", b"tort", 10 * COST_EDIT),
            Some(COST_EDIT + COST_INITIAL)
        );
        // … while the same substitution in the middle is just the edit.
        assert_eq!(
            dp.distance(b"tosrt", b"tobrt", 10 * COST_EDIT),
            Some(COST_EDIT)
        );
        // A transposed opening is exempt: every letter survives, reordered.
        assert_eq!(dp.distance(b"hte", b"the", 10 * COST_EDIT), Some(COST_TRANSPOSE));
    }

    #[test]
    fn edit_costs_rank_the_likely_slips_below_a_plain_edit() {
        let mut dp = Dp::default();
        let mut cost = |a: &str, b: &str| dp.distance(a.as_bytes(), b.as_bytes(), 10 * COST_EDIT);
        assert_eq!(cost("helo", "hello"), Some(COST_DOUBLE), "restores a double");
        assert_eq!(cost("hellp", "help"), Some(COST_DOUBLE), "un-doubles");
        assert_eq!(cost("hleo", "helo"), Some(COST_TRANSPOSE), "transposition");
        assert_eq!(
            cost("wans", "wand"),
            Some(COST_ADJACENT_ROW),
            "the key beside it in the same row"
        );
        assert_eq!(
            cost("wang", "want"),
            Some(COST_ADJACENT_DIAG),
            "a key one row over"
        );
        // An extra letter next to the key beside it: two keys caught at once,
        // not a letter the writer believed in.
        assert_eq!(cost("worjk", "work"), Some(COST_STRAY_KEY), "stray key");
        assert_eq!(
            cost("worqk", "work"),
            Some(COST_EDIT),
            "an extra letter from nowhere near is still a full edit"
        );
        assert_eq!(cost("mork", "mort"), Some(COST_EDIT), "unrelated letter");
        assert_eq!(cost("hello", "hello"), Some(0), "no edit");
    }

    #[test]
    fn the_prior_is_a_factor_not_a_tie_break() {
        // Ten times more common is worth about a third of a plain edit …
        let decade = score(0, 990) - score(0, 90);
        assert!(
            (15.0..25.0).contains(&decade),
            "a decade of rank is worth {decade} cost units"
        );
        // … so it can overturn a small channel difference but never a whole
        // extra edit.
        assert!(score(COST_DOUBLE, 40_000) > score(COST_ADJACENT_ROW, 5));
        // And a whole extra plain edit is never bought by frequency: the
        // cheapest candidate at the very bottom of the 50k list still beats a
        // one-edit-dearer candidate at the very top.
        assert!(score(COST_EDIT, 0) > score(0, 50_000));
    }

    #[test]
    fn distance_bails_out_past_the_budget() {
        let mut dp = Dp::default();
        assert_eq!(dp.distance(b"hello", b"zzzzzzzz", COST_EDIT), None);
        // The bail-out must not change the answer for anything inside budget.
        assert_eq!(dp.distance(b"helo", b"hello", COST_EDIT), Some(COST_DOUBLE));
    }

    #[test]
    fn the_bag_bound_never_overstates_the_distance() {
        // The pre-filter is only sound if it can't reject a candidate the real
        // distance would have accepted, so check it against the matrix itself.
        let mut dp = Dp::default();
        for (a, b) in [
            ("helo", "hello"),
            ("hellp", "help"),
            ("theri", "their"),
            ("beutifull", "beautiful"),
            ("keboadr", "keyboard"),
            ("adress", "address"),
            // The stray-key discount is a cheaper edit than the bound knew
            // about before it was derived from the table — check it here too.
            ("worjk", "work"),
            ("mnake", "make"),
            ("fone", "phone"),
            ("dependant", "dependent"),
            ("hello", "hello"),
        ] {
            let real = dp
                .distance(a.as_bytes(), b.as_bytes(), 10 * COST_EDIT)
                .expect("within budget");
            let bound = bag_bound(&letter_counts(a.as_bytes()), b.as_bytes());
            assert!(bound <= real, "{a} -> {b}: bound {bound} > real {real}");
        }
        // And it does reject the obviously unrelated.
        assert!(bag_bound(&letter_counts(b"hello"), b"zqxjkvw") > COST_EDIT);
    }

    #[test]
    fn the_rule_table_stays_within_what_the_matrix_assumes() {
        for rule in RULES {
            assert!(!rule.from.is_empty(), "{:?} needs a typed side", rule.to);
            assert!(rule.from.len() <= MAX_RULE_LEN, "{} too long", rule.from);
            assert!(rule.to.len() <= MAX_RULE_LEN, "{} too long", rule.to);
            assert!(rule.from != rule.to, "{} is not an edit", rule.from);
            for side in [rule.from, rule.to] {
                assert!(
                    side.bytes().all(|b| b.is_ascii_lowercase()),
                    "{side} is not typeable"
                );
            }
            // A rule that undercuts the letter-bag bound would break pruning;
            // the bound derives its rate from this same table, so this only
            // pins that the derivation ran.
            assert!(rule.cost > 0);
        }
        assert!(min_cost_per_letter_x100() > 0);
    }

    #[test]
    fn adjacency_follows_the_keyboard() {
        assert!(adjacent(b'g', b'f'), "same row");
        assert!(adjacent(b'g', b't'), "row above");
        assert!(adjacent(b'g', b'b'), "row below");
        assert!(!adjacent(b'g', b'p'), "across the keyboard");
        assert!(!adjacent(b'a', b'a'), "a key is not its own neighbour");
    }

    #[test]
    fn sliding_along_a_row_is_likelier_than_reaching_across_one() {
        assert_eq!(adjacent_cost(b'g', b'f'), Some(COST_ADJACENT_ROW));
        assert_eq!(adjacent_cost(b'g', b't'), Some(COST_ADJACENT_DIAG));
        assert_eq!(adjacent_cost(b'g', b'b'), Some(COST_ADJACENT_DIAG));
        assert_eq!(adjacent_cost(b'g', b'p'), None);
        const { assert!(COST_ADJACENT_ROW < COST_ADJACENT_DIAG) };
    }

    #[test]
    fn an_extra_letter_is_priced_by_what_it_could_have_come_from() {
        // Half of a double, typed twice.
        assert_eq!(extra_cost(b"hellp", 4), COST_DOUBLE);
        // A neighbour of the key beside it: two keys caught at once. Checked
        // in both directions, since the hand can catch the extra key on the
        // way in or on the way out.
        assert_eq!(extra_cost(b"worjk", 4), COST_STRAY_KEY, "next to the letter after");
        assert_eq!(extra_cost(b"mnake", 2), COST_STRAY_KEY, "next to the letter before");
        // A letter from the other side of the keyboard explains nothing about
        // the hand, so it stays a misspelling.
        assert_eq!(extra_cost(b"worqk", 4), COST_EDIT);
    }

    #[test]
    fn a_dropped_letter_is_not_explained_by_the_keyboard() {
        // The asymmetry: adjacency is about keys that were *hit*. "make" is
        // missing nothing because of where "n" sits, so restoring a letter
        // next to its neighbour is still a plain edit — only the double-letter
        // discount applies on this side.
        assert_eq!(missing_cost(b"hello", 4), COST_DOUBLE);
        assert_eq!(missing_cost(b"make", 2), COST_EDIT);
    }

    #[test]
    fn openings_cover_the_reachable_first_letters() {
        // The typed letter, the transposed opening, and whatever a word-initial
        // rule could produce.
        let letters = |w: &str| openings(w).iter().map(|o| o.letter).collect::<Vec<_>>();
        assert_eq!(letters("helo"), vec![b'h', b'e']);
        assert!(letters("fone").contains(&b'p'), "f -> ph");
        assert!(letters("nife").contains(&b'k'), "n -> kn");
        // A doubled opening contributes its letter once.
        assert_eq!(letters("aardvark"), vec![b'a']);
        // And each run only admits what it was opened for: the second-letter
        // run is for transposed openings and nothing else.
        let swap = openings("hte")[1];
        assert!(swap.admits(b"hte", b"the"));
        assert!(!swap.admits(b"hte", b"time"), "not a transposed opening");
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
        correct_with(
            word,
            en_dict(),
            en_freq(),
            crate::config::DEFAULT_SPELL_MIN_LEN,
            crate::config::DEFAULT_SPELL_MAX_RANK,
            crate::config::DEFAULT_SPELL_MAX_DIST,
        )
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
    fn fixes_fat_finger_typos() {
        // Slips of the hand rather than of the memory: a key caught on the way
        // past, or the one next to the one meant. None of these are spelling
        // mistakes — the writer knows the word — and what makes them reachable
        // is knowing which keys sit next to which.
        for (typo, want) in [
            ("worjk", "work"),
            ("mnake", "make"),
            ("tjhat", "that"),
            ("witjh", "with"),
            ("abnout", "about"),
            ("juest", "just"),
            ("alsdo", "also"),
            ("yearsd", "years"),
            ("thiunk", "think"),
            ("peopkle", "people"),
            ("conmputer", "computer"),
            ("becausse", "because"),
            // Substituting the neighbouring key rather than adding one.
            ("wprk", "work"),
            ("tjis", "this"),
            ("fimd", "find"),
            ("srill", "still"),
            ("differemt", "different"),
        ] {
            assert_eq!(fix(typo).as_deref(), Some(want), "{typo}");
        }
    }

    #[test]
    fn fixes_badly_mangled_words() {
        // The words a one-edit speller had to give up on: two or three slips in
        // the same word, which only the length-scaled budget can reach.
        for (typo, want) in [
            ("recieveing", "receiving"),
            ("seperatly", "separately"),
            ("begining", "beginning"),
            ("necesary", "necessary"),
            ("occassion", "occasion"),
            ("acheivment", "achievement"),
            ("emabrassed", "embarrassed"),
            ("comitte", "committee"),
            ("existance", "existence"),
            ("independant", "independent"),
        ] {
            assert_eq!(fix(typo).as_deref(), Some(want), "{typo}");
        }
    }

    #[test]
    fn fixes_spelling_confusions_a_letter_model_cannot_reach() {
        // The Brill–Moore rules earning their place: each of these is one
        // decision the writer made about how the word is spelled, and a
        // single-character model would have to buy it a letter at a time.
        for (typo, want) in [
            ("apparant", "apparent"),
            ("diferent", "different"),
            ("independance", "independence"),
            ("persistant", "persistent"),
            ("maintainance", "maintenance"),
            ("arguement", "argument"),
            ("concious", "conscious"),
            ("buisness", "business"),
            ("restaraunt", "restaurant"),
            // The rules reach a phonetic respelling a letter model cannot:
            // "fisical" and "physical" share barely half their letters.
            ("fisical", "physical"),
        ] {
            assert_eq!(fix(typo).as_deref(), Some(want), "{typo}");
        }
        // Not everything: "occurance" → "occurrence" is a rule plus a doubled
        // "r", and at two edits the rank budget (max_rank / 2) is tighter than
        // "occurrence" is common. A miss is invisible; a wrong fix is not.
        assert_eq!(fix("occurance"), None);
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


