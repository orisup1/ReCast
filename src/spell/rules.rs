//! Multi-character English spelling confusions used by the edit channel.

use std::sync::OnceLock;

/// A generic string-to-string edit: typed `from`, intended `to`.
pub(super) struct Rule {
    pub(super) from: &'static str,
    pub(super) to: &'static str,
    pub(super) cost: u32,
}

const fn rule(from: &'static str, to: &'static str, cost: u32) -> Rule {
    Rule { from, to, cost }
}

pub(super) const MAX_RULE_LEN: usize = 4;
pub(super) const LOOKBACK: usize = MAX_RULE_LEN;
pub(super) const COST_SPELLING: u32 = 70;
pub(super) const COST_HOMOPHONE: u32 = 55;

/// String-to-string edits, `typed` → `intended`.
///
/// Neither side may exceed [`MAX_RULE_LEN`], and `from` must not be empty.
pub(super) static RULES: &[Rule] = &[
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

/// Rules indexed by the final byte of their typed side.
pub(super) fn rules_by_last_byte() -> &'static [Vec<&'static Rule>; 26] {
    static INDEX: OnceLock<[Vec<&'static Rule>; 26]> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index: [Vec<&'static Rule>; 26] = std::array::from_fn(|_| Vec::new());
        for rule in RULES {
            let last = *rule
                .from
                .as_bytes()
                .last()
                .expect("a rule needs a typed side");
            index[(last - b'a') as usize].push(rule);
        }
        index
    })
}
