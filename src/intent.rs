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
// A conditional share of at least 5% permits at most twenty successors.
const MAX_PHRASE_SUCCESSORS: usize = 20;

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

    fn successors(self, context: &str) -> impl Iterator<Item = (&'static str, u32)> {
        let prefix = format!("{context} ");
        self.0[self.lower_bound(&prefix)..]
            .lines()
            .map_while(move |row| {
                let (key, counts) = row.split_once('\t')?;
                Some((key.strip_prefix(&prefix)?, counts))
            })
            .filter_map(|(word, counts)| {
                if word.contains(' ') {
                    return None;
                }
                Some((word, counts.split('\t').next()?.parse().ok()?))
            })
    }
}

#[derive(Default)]
struct Likelihood {
    conditional: f64,
    baseline: f64,
}

/// Trusted visible words from one uninterrupted typing context. Nothing here
/// is written to disk, even when persistent personalization is enabled.
pub(crate) struct Context {
    words: VecDeque<String>,
    // Cached context counts avoid repeating their lookups for every candidate.
    suffixes: Vec<(String, u32)>,
    total: u32,
    model: Model,
    likelihoods: HashMap<&'static str, Likelihood>,
    backoff: f64,
    phrase_priors: HashMap<&'static str, f64>,
}

impl Default for Context {
    fn default() -> Self {
        Self {
            words: VecDeque::new(),
            suffixes: Vec::new(),
            total: 0,
            model: Model::bundled(),
            likelihoods: HashMap::new(),
            backoff: 1.0,
            phrase_priors: HashMap::new(),
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
        self.likelihoods.clear();
        self.backoff = 1.0;
        self.phrase_priors.clear();
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
        self.prepare_phrase_priors();
        self.prepare_factors();
    }

    /// Cache the small build-time-filtered successor range once per word.
    /// No full-table startup parsing or candidate-time corpus searches are needed.
    fn prepare_phrase_priors(&mut self) {
        self.phrase_priors.clear();
        let Some(previous) = self.words.back() else {
            return;
        };
        let phrases = Model(include_str!(concat!(
            env!("OUT_DIR"),
            "/offer_phrases.blob"
        )));
        let prefix = format!("{previous} ");
        for row in phrases.0[phrases.lower_bound(&prefix)..].lines() {
            let Some((key, weight)) = row.split_once('\t') else {
                break;
            };
            let Some(word) = key.strip_prefix(&prefix) else {
                break;
            };
            if let Ok(weight) = weight.parse() {
                self.phrase_priors.insert(word, weight);
            }
            if self.phrase_priors.len() > MAX_PHRASE_SUCCESSORS {
                self.phrase_priors.clear();
                return;
            }
        }
    }

    /// A bounded previous-word prior, independent of longer-context factors.
    /// Automatic rewriting intentionally keeps its existing evidence policy.
    pub(crate) fn phrase_prior(&self, word: &str) -> f64 {
        self.phrase_priors.get(word).copied().unwrap_or(1.0)
    }

    /// Cache the conditional distribution once per committed word. Candidate
    /// search then needs one hash lookup rather than several binary searches
    /// and temporary strings for every possible typo alignment. Compare with
    /// the one-word distribution so longer history adds only new evidence to
    /// the phrase priors that already use the previous word.
    fn prepare_factors(&mut self) {
        self.likelihoods.clear();
        self.backoff = 1.0;
        // Empty longer histories are neutral. Avoid preparing a potentially
        // large one-word distribution when it would only be divided by itself.
        if self.total == 0
            || !self
                .suffixes
                .iter()
                .any(|(key, _)| key.contains(' ') && self.model.successors(key).next().is_some())
        {
            return;
        }
        let mut baseline_backoff = 1.0;
        for (index, (context, total)) in self.suffixes.iter().enumerate() {
            let successors: Vec<_> = self
                .model
                .successors(context)
                .take(MAX_SUCCESSORS + 1)
                .collect();
            if successors.len() > MAX_SUCCESSORS {
                self.likelihoods.clear();
                self.backoff = 1.0;
                return;
            }
            let retained: u64 = successors.iter().map(|(_, count)| u64::from(*count)).sum();
            let denominator = f64::from(*total) + SMOOTHING;
            // Counts omitted by pruning (and sentence-final contexts) carry
            // no evidence about the next word. Return their probability mass
            // to the shorter context instead of treating it as negative evidence.
            let discount = 1.0 - retained as f64 / denominator;
            self.backoff *= discount;
            for likelihood in self.likelihoods.values_mut() {
                likelihood.conditional *= discount;
            }
            for (word, count) in successors {
                self.likelihoods.entry(word).or_default().conditional +=
                    f64::from(count) / denominator;
                if self.likelihoods.len() > MAX_SUCCESSORS {
                    break;
                }
            }
            if self.likelihoods.len() > MAX_SUCCESSORS {
                // Fall back uniformly instead of biasing a truncated vocabulary.
                self.likelihoods.clear();
                self.backoff = 1.0;
                return;
            }
            if index == 0 {
                baseline_backoff = self.backoff;
                for likelihood in self.likelihoods.values_mut() {
                    likelihood.baseline = likelihood.conditional;
                }
            }
        }
        for (word, likelihood) in &mut self.likelihoods {
            let unigram = self.model.row(word).map_or(1, |(count, _)| count);
            let prior = f64::from(unigram) / f64::from(self.total);
            likelihood.conditional += self.backoff * prior;
            likelihood.baseline += baseline_backoff * prior;
        }
        self.backoff = (self.backoff / baseline_backoff).clamp(0.25, 8.0);
    }

    /// Smoothed likelihood relative to the one-word context. Sparse histories
    /// back off without duplicating or suppressing the existing phrase priors.
    pub(crate) fn factor(&self, word: &str) -> f64 {
        if self.total == 0
            || self
                .previous_word()
                .is_none_or(|last| last.is_ascii() != word.is_ascii())
            || !self.suffixes.iter().any(|(key, _)| key.contains(' '))
        {
            return 1.0;
        }
        self.likelihoods
            .get(word)
            .map_or(self.backoff, |likelihood| {
                (likelihood.conditional / likelihood.baseline).clamp(0.25, 8.0)
            })
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
        assert!((context.factor("bank") - 1.7660818713450293).abs() < 1e-9);
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
    fn offer_phrase_table_preserves_raw_lexical_trust_and_evidence_thresholds() {
        use std::collections::HashSet;

        let mut allowed = HashSet::new();
        for (dictionary, frequency) in [
            (
                include_str!("../en_dict.txt"),
                include_str!("../en_freq.txt"),
            ),
            (
                include_str!("../he_dict.txt"),
                include_str!("../he_freq.txt"),
            ),
        ] {
            let dictionary: HashSet<_> = dictionary.lines().map(str::to_lowercase).collect();
            allowed.extend(
                frequency
                    .lines()
                    .take(20_000)
                    .map(str::to_lowercase)
                    .filter(|word| {
                        dictionary.contains(word) && (2..=20).contains(&word.chars().count())
                    }),
            );
        }
        // Runtime typing blobs accept these quote-stripped aliases. They must
        // not inherit their earlier, quoted frequency entries' phrase trust.
        for word in ["argo", "nt"] {
            assert!(crate::dictionary::en_dict().contains(word));
            assert!(crate::dictionary::en_freq().rank(word).unwrap() < 20_000);
            assert!(!allowed.contains(word));
        }
        assert!(crate::dictionary::en_dict().contains("ssh"));
        assert!(
            !allowed.contains("ssh"),
            "technical additions are not public phrase trust"
        );
        for word in ["spunk", "המשרדים"] {
            assert!(
                !allowed.contains(word),
                "raw zero-based rank 20,000 is excluded"
            );
        }

        let model = Model::bundled();
        let table = include_str!(concat!(env!("OUT_DIR"), "/offer_phrases.blob"));
        let mut actual = HashMap::new();
        let mut successors = HashMap::<_, usize>::new();
        let mut previous_key = "";
        for row in table.lines() {
            let (key, weight) = row.split_once('\t').unwrap();
            assert!(
                previous_key < key,
                "offer table must remain sorted and unique"
            );
            previous_key = key;
            let (previous, next) = key.split_once(' ').unwrap();
            assert!(allowed.contains(previous) && allowed.contains(next));
            let (count, previous_count) = model.row(key).unwrap();
            let (_, total) = model.row(previous).unwrap();
            let (next_count, _) = model.row(next).unwrap();
            assert!(count >= 5);
            assert!(f64::from(count) / f64::from(previous_count) >= 0.05);
            let association = f64::from(count) * f64::from(total)
                / (f64::from(previous_count) * f64::from(next_count));
            assert!(association >= 3.0);
            let weight: f64 = weight.parse().unwrap();
            assert_eq!(weight, (1.0 + association.log2()).min(4.0));
            assert!(actual.insert(key, weight).is_none());
            *successors.entry(previous).or_default() += 1;
        }
        assert_eq!(actual.len(), 1537);
        assert!(successors
            .values()
            .all(|&count| count <= MAX_PHRASE_SUCCESSORS));
        // Each rejected pair meets the other evidence thresholds. Their true
        // corpus counts exercise count, conditional share, association and rank.
        for pair in ["accounted for", "about an", "above the", "acquiring an"] {
            assert!(!actual.contains_key(pair), "ineligible pair: {pair}");
        }
        for (previous, next) in [("worry", "about"), ("work", "together"), ("very", "well")] {
            let mut context = Context::default();
            context.observe(previous.into());
            assert_eq!(
                context.phrase_prior(next),
                actual[&*format!("{previous} {next}")]
            );
            assert!(context.phrase_prior(next) > 1.0);
            assert_eq!(context.factor(next), 1.0);
            context.observe("unmeasuredword".into());
            assert_eq!(context.phrase_prior(next), 1.0);
            context.clear();
            assert!(context.phrase_priors.is_empty());
        }
    }

    #[test]
    fn unretained_continuations_leave_shorter_context_evidence_intact() {
        for [earlier, shared, last, favored, other] in [
            ["earlier", "shared", "context", "bank", "band"],
            ["קודם", "משותף", "הקשר", "בנק", "להקה"],
        ] {
            let mut rows = vec![
                format!("{earlier}\t100\t1000"),
                format!("{shared}\t100\t1000"),
                format!("{last}\t100\t1000"),
                format!("{favored}\t20\t1000"),
                format!("{other}\t20\t1000"),
                format!("{earlier} {shared}\t8\t100"),
                format!("{earlier} {shared} {last}\t4\t8"),
                format!("{shared} {last}\t16\t100"),
                format!("{shared} {last} {favored}\t12\t16"),
                format!("{last} {favored}\t12\t100"),
                format!("{last} {other}\t12\t100"),
            ];
            rows.sort_unstable();
            let model = Box::leak(rows.join("\n").into_boxed_str());
            let mut shorter = Context::with_model(model);
            for word in [shared, last] {
                shorter.observe(word.into());
            }
            let mut longer = Context::with_model(model);
            for word in [earlier, shared, last] {
                longer.observe(word.into());
            }
            assert!(shorter.factor(favored) > shorter.factor(other));
            for word in [favored, other, "unknown", "אלמוני"] {
                assert_eq!(longer.factor(word), shorter.factor(word), "{word}");
            }
        }
    }

    #[test]
    fn longer_history_without_retained_successors_is_neutral() {
        let model = "a\t100\t1000\na b\t8\t100\nb\t100\t1000\nb c\t20\t100\nb d\t20\t100\nc\t100\t1000\nd\t100\t1000\n";
        let mut context = Context::with_model(model);
        for word in ["a", "b"] {
            context.observe(word.into());
        }
        for word in ["c", "d", "unseen"] {
            assert_eq!(context.factor(word), 1.0, "{word}");
        }
    }

    #[test]
    fn interpolation_returns_pruned_mass_to_the_shorter_distribution() {
        let model = "a\t100\t1000\na b\t8\t100\na b c\t4\t8\nb\t100\t1000\nb c\t20\t100\nb d\t20\t100\nc\t100\t1000\nd\t100\t1000\n";
        let mut context = Context::with_model(model);
        for word in ["a", "b"] {
            context.observe(word.into());
        }
        // Only four of the eight continuations survived pruning. Along with
        // eight smoothing counts, the missing four back off: 12 / 16, not 8 / 16.
        let baseline = 20.0 / 108.0 + (68.0 / 108.0) * 0.1;
        assert!((context.factor("c") - (0.75 * baseline + 0.25) / baseline).abs() < 1e-9);
        assert!((context.factor("d") - 0.75).abs() < 1e-9);
        assert!((context.factor("unseen") - 0.75).abs() < 1e-9);
    }

    #[test]
    fn bundled_sparse_contexts_do_not_invent_negative_evidence() {
        for words in [["on", "monday"], ["עם", "זאת"]] {
            let mut context = Context::default();
            for word in words {
                context.observe(word.into());
            }
            assert_eq!(context.suffixes.len(), 2);
            for word in ["morning", "unknown", "היום", "אלמוני"] {
                assert_eq!(context.factor(word), 1.0, "{words:?}: {word}");
            }
        }
    }

    #[test]
    fn excessive_successor_vocabulary_abstains_without_truncation() {
        let mut rows = vec![
            "a\t10000\t40000".to_owned(),
            "a b\t10000\t10000".to_owned(),
            "a b word0000\t2\t10000".to_owned(),
            "b\t10000\t40000".to_owned(),
        ];
        for index in 0..=MAX_SUCCESSORS {
            rows.push(format!("b word{index:04}\t2\t10000"));
            rows.push(format!("word{index:04}\t2\t40000"));
        }
        rows.sort_unstable();
        let model = Box::leak(rows.join("\n").into_boxed_str());
        let mut context = Context::with_model(model);
        for word in ["a", "b"] {
            context.observe(word.into());
        }
        assert!(context.likelihoods.is_empty());
        for word in ["word0000", "word4096", "unknown"] {
            assert_eq!(context.factor(word), 1.0, "{word}");
        }
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
