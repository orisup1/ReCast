//! Local context likelihoods and a shared reject option for inferred edits.
//!
//! The bundled counts are measured independently of the correction corpus.
//! Scores remain policy costs, not calibrated probabilities of user intent.

use std::collections::{HashMap, VecDeque};

const MEMORY: usize = 16;
const ORDER: usize = 4;
const MIN_CONTEXT_COUNT: u32 = 4;
const SMOOTHING: f64 = 8.0;
const MAX_SUCCESSORS: usize = 4096;

#[derive(Clone, Copy)]
struct Model(&'static str);

impl Model {
    fn bundled() -> Self {
        let data = include_str!("intent_counts.tsv");
        Self(data.trim_start_matches('\u{feff}').trim_start())
    }

    /// Search newline-aligned bytes so a probe never slices a Hebrew character.
    fn lower_bound(self, key: &str) -> usize {
        let data = self.0;
        let bytes = data.as_bytes();
        let mut low = 0;
        while bytes.get(low) == Some(&b'#') {
            let Some(end) = bytes[low..].iter().position(|&b| b == b'\n') else {
                return bytes.len();
            };
            low += end + 1;
        }
        let mut high = bytes.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let start = bytes[low..middle]
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(low, |offset| low + offset + 1);
            let end = bytes[start..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(bytes.len(), |offset| start + offset);
            let row_key = data[start..end].split('\t').next().unwrap_or_default();
            if row_key < key {
                low = end.saturating_add(1).min(bytes.len());
            } else {
                high = start;
            }
        }
        low
    }

    fn row(self, key: &str) -> Option<(u32, u32)> {
        let mut fields = self.0[self.lower_bound(key)..].lines().next()?.split('\t');
        (fields.next()? == key).then_some(())?;
        Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
    }

    fn successors(self, context: &str, mut visit: impl FnMut(&'static str, u32)) {
        let prefix = format!("{context} ");
        for row in self.0[self.lower_bound(&prefix)..].lines() {
            let mut fields = row.split('\t');
            let Some(word) = fields.next().and_then(|key| key.strip_prefix(&prefix)) else {
                break;
            };
            if !word.contains(' ') {
                if let Some(count) = fields.next().and_then(|count| count.parse().ok()) {
                    visit(word, count);
                }
            }
        }
    }
}

/// Trusted visible words from one uninterrupted typing context. Nothing here
/// is written to disk, even when persistent personalization is enabled.
pub(crate) struct Context {
    words: VecDeque<String>,
    // Cached context counts avoid repeating their lookups for every candidate.
    suffixes: Vec<(String, u32)>,
    total: u32,
    model: Model,
    factors: HashMap<&'static str, f64>,
    backoff: f64,
}

impl Default for Context {
    fn default() -> Self {
        Self {
            words: VecDeque::new(),
            suffixes: Vec::new(),
            total: 0,
            model: Model::bundled(),
            factors: HashMap::new(),
            backoff: 1.0,
        }
    }
}

impl Context {
    pub(crate) fn previous_word(&self) -> Option<&str> {
        self.words.back().map(String::as_str)
    }

    pub(crate) fn clear(&mut self) {
        self.words.clear();
        self.suffixes.clear();
        self.total = 0;
        self.factors.clear();
        self.backoff = 1.0;
    }

    pub(crate) fn observe(&mut self, word: String) {
        self.words.push_back(word);
        if self.words.len() > MEMORY {
            self.words.pop_front();
        }
        self.suffixes.clear();
        self.total = 0;
        let last = self.words.back().expect("a word was appended");
        let english = last.is_ascii();
        let words: Vec<_> = self
            .words
            .iter()
            .rev()
            .take(ORDER - 1)
            .take_while(|word| word.is_ascii() == english)
            .collect();
        if let Some((_, total)) = self.model.row(last) {
            self.total = total;
        }
        for length in 1..=words.len() {
            let key = words[..length]
                .iter()
                .rev()
                .map(|word| word.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if let Some((count, _)) = self.model.row(&key) {
                if count >= MIN_CONTEXT_COUNT {
                    self.suffixes.push((key, count));
                }
            }
        }
        self.prepare_factors();
    }

    /// Cache the conditional distribution once per committed word. Candidate
    /// search then needs one hash lookup rather than several binary searches
    /// and temporary strings for every possible typo alignment.
    fn prepare_factors(&mut self) {
        self.factors.clear();
        self.backoff = 1.0;
        if self.total == 0 || !self.suffixes.iter().any(|(key, _)| key.contains(' ')) {
            return;
        }
        for (context, total) in &self.suffixes {
            let denominator = f64::from(*total) + SMOOTHING;
            let discount = SMOOTHING / denominator;
            self.backoff *= discount;
            for mass in self.factors.values_mut() {
                *mass *= discount;
            }
            self.model.successors(context, |word, count| {
                if self.factors.len() <= MAX_SUCCESSORS {
                    *self.factors.entry(word).or_default() += f64::from(count) / denominator;
                }
            });
            if self.factors.len() > MAX_SUCCESSORS {
                // Fall back uniformly instead of biasing a truncated vocabulary.
                self.factors.clear();
                self.backoff = 1.0;
                return;
            }
        }
        for (word, mass) in &mut self.factors {
            let unigram = self.model.row(word).map_or(1, |(count, _)| count);
            *mass = (self.backoff + *mass * f64::from(self.total) / f64::from(unigram))
                .clamp(0.25, 8.0);
        }
        self.backoff = self.backoff.clamp(0.25, 8.0);
    }

    /// Smoothed conditional likelihood relative to the unigram prior. Sparse
    /// contexts back off; at least two preceding words must provide evidence
    /// before this supplements the existing one-word phrase priors.
    pub(crate) fn factor(&self, word: &str) -> f64 {
        if self.total == 0
            || self
                .previous_word()
                .is_none_or(|last| last.is_ascii() != word.is_ascii())
            || !self.suffixes.iter().any(|(key, _)| key.contains(' '))
        {
            return 1.0;
        }
        self.factors.get(word).copied().unwrap_or(self.backoff)
    }

    #[cfg(test)]
    pub(crate) fn with_model(data: &'static str) -> Self {
        Self {
            model: Model(data),
            ..Self::default()
        }
    }
}

/// Select only a finite winner with the required lead over every alternative.
/// Callers include keeping the original as an ordinary candidate at index zero.
pub(crate) fn select(scores: &[f32], minimum_lead: f32) -> Option<usize> {
    if scores.len() < 2 || scores.iter().any(|score| !score.is_finite()) {
        return None;
    }
    let (winner, best) = scores
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| a.total_cmp(b))?;
    scores
        .iter()
        .enumerate()
        .all(|(index, score)| index == winner || score - best >= minimum_lead)
        .then_some(winner)
}

/// Preserve each model's measured cost margin when comparing actions across
/// languages. The cap prevents a single heuristic from overwhelming feedback.
pub(crate) fn normalized_lead(scores: &[f32], winner: usize, minimum_lead: f32) -> f32 {
    let Some(best) = scores.get(winner) else {
        return 0.0;
    };
    let margin = scores
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != winner)
        .map(|(_, score)| score - best)
        .fold(f32::INFINITY, f32::min);
    (4.0 * margin / minimum_lead).clamp(0.0, 16.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODEL: &str = "at\t100\t1000\nat the\t100\t100\nat the bank\t12\t100\nband\t20\t1000\nbank\t20\t1000\njoined\t100\t1000\njoined the\t100\t100\njoined the band\t12\t100\nthe\t100\t1000\nthe band\t12\t100\nthe bank\t12\t100\n";

    #[test]
    fn longer_context_distinguishes_the_same_prefix_and_previous_word() {
        let mut context = Context::with_model(MODEL);
        for word in ["at", "the"] {
            context.observe(word.into());
        }
        assert!((context.factor("bank") - 5.972565157750343).abs() < 1e-9);
        assert!(context.factor("bank") > context.factor("band"));
        context.clear();
        for word in ["joined", "the"] {
            context.observe(word.into());
        }
        assert!(context.factor("band") > context.factor("bank"));
        assert!(context.factor("bank") < 1.0);
        context.clear();
        assert_eq!(context.factor("bank"), 1.0);
    }

    #[test]
    fn four_grams_distinguish_contexts_with_the_same_last_two_words() {
        let model = concat!(
            "band\t20\t1000\nbank\t20\t1000\nby\t100\t1000\nby the\t100\t100\n",
            "by the band\t12\t100\nby the bank\t12\t100\n",
            "music\t100\t1000\nmusic by\t100\t100\nmusic by the\t100\t100\n",
            "music by the band\t12\t100\nstopped\t100\t1000\nstopped by\t100\t100\n",
            "stopped by the\t100\t100\nstopped by the bank\t12\t100\n",
            "the\t100\t1000\nthe band\t12\t100\nthe bank\t12\t100\n",
        );
        let mut context = Context::with_model(model);
        for word in ["stopped", "by", "the"] {
            context.observe(word.into());
        }
        assert!(context.factor("bank") > context.factor("band"));
        context.clear();
        for word in ["music", "by", "the"] {
            context.observe(word.into());
        }
        assert!(context.factor("band") > context.factor("bank"));
    }

    #[test]
    fn memory_and_sparse_evidence_are_bounded() {
        let mut context = Context::with_model(MODEL);
        for _ in 0..100 {
            context.observe("unknown".into());
        }
        assert_eq!(context.words.len(), MEMORY);
        assert_eq!(context.factor("bank"), 1.0);
        context.observe("שלום".into());
        assert_eq!(context.factor("bank"), 1.0);
    }

    #[test]
    fn bundled_model_is_sorted_searchable_and_keeps_true_denominators() {
        let model = Model::bundled();
        let mut previous = "";
        let mut orders = [0; ORDER];
        for row in model.0.lines().filter(|row| !row.starts_with('#')) {
            let mut fields = row.split('\t');
            let key = fields.next().unwrap();
            assert!(previous < key, "unsorted or duplicate key {key:?}");
            previous = key;
            let count: u32 = fields.next().unwrap().parse().unwrap();
            let total: u32 = fields.next().unwrap().parse().unwrap();
            assert!(count >= 2 && count <= total);
            let order = key.split(' ').count();
            assert!((1..=ORDER).contains(&order));
            orders[order - 1] += 1;
            assert_eq!(model.row(key), Some((count, total)));
            if let Some((prefix, _)) = key.rsplit_once(' ') {
                assert_eq!(model.row(prefix).unwrap().0, total);
            }
        }
        assert!(orders.iter().all(|count| *count > 100));
        assert_eq!(model.row("not a measured corpus phrase"), None);
    }

    #[test]
    fn rejection_compares_original_and_every_runner_up() {
        assert_eq!(select(&[0.0, -5.0, -1.0], 4.0), Some(1));
        assert_eq!(select(&[0.0, -3.0, 10.0], 4.0), None);
        assert_eq!(select(&[0.0, -5.0, -5.0], 4.0), None);
        assert_eq!(select(&[0.0, f32::NAN], 4.0), None);
    }
}
