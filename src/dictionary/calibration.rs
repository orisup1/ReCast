//! Offline, test-only threshold experiments over synthetic labeled text.

use super::*;
use std::cell::Cell;
use std::collections::BTreeMap;

thread_local! {
    static POLICY: Cell<(&'static str, f32)> = const { Cell::new(("baseline", 1.0)) };
}

pub(crate) fn threshold(gate: &str, default: f32) -> f32 {
    POLICY.with(|policy| {
        let (selected, scale) = policy.get();
        default * if selected == gate { scale } else { 1.0 }
    })
}

fn with_policy<T>(gate: &'static str, scale: f32, run: impl FnOnce() -> T) -> T {
    assert!(scale.is_finite() && scale > 0.0);
    struct Restore((&'static str, f32));
    impl Drop for Restore {
        fn drop(&mut self) {
            POLICY.with(|policy| policy.set(self.0));
        }
    }
    let _restore = Restore(POLICY.with(|policy| policy.replace((gate, scale))));
    run()
}

struct Case<'a> {
    line: usize,
    current: Language,
    en: &'a str,
    he: &'a str,
    expected: &'a str,
    sequence: &'a str,
    layout_only: bool,
}

impl Case<'_> {
    fn before(&self) -> &str {
        if self.current == Language::English {
            self.en
        } else {
            self.he
        }
    }
}

fn parse(corpus: &str) -> Result<Vec<Case<'_>>, String> {
    let mut cases = Vec::new();
    for (index, row) in corpus.lines().enumerate() {
        if row.is_empty() || row.starts_with('#') {
            continue;
        }
        let line = index + 1;
        let fields: Vec<_> = row.split('\t').collect();
        let invalid = || format!("invalid calibration corpus row {line}");
        if !matches!(fields.len(), 4 | 5 | 7) {
            return Err(invalid());
        }
        let current = match fields[0] {
            "en" => Language::English,
            "he" => Language::Hebrew,
            _ => return Err(invalid()),
        };
        if fields[1..4].iter().any(|s| s.is_empty())
            || fields[1].chars().count() != fields[2].chars().count()
        {
            return Err(invalid());
        }
        let layout_only = match fields.get(6).copied().unwrap_or("full") {
            "full" => false,
            "layout-only" => true,
            _ => return Err(invalid()),
        };
        cases.push(Case {
            line,
            current,
            en: fields[1],
            he: fields[2],
            expected: fields[3],
            sequence: fields.get(5).copied().unwrap_or(""),
            layout_only,
        });
    }
    if cases.is_empty() {
        return Err("empty calibration corpus".into());
    }
    Ok(cases)
}

#[derive(Debug, PartialEq)]
struct Observation {
    after: String,
    abstain: Option<&'static str>,
}

fn replay(cases: &[Case<'_>], context: bool) -> Vec<Observation> {
    let mut history = History::default();
    let mut phrase = crate::complete::PhraseContext::default();
    let mut sequence = "";
    let mut previous_mode = false;
    cases
        .iter()
        .map(|case| {
            if case.sequence.is_empty()
                || case.sequence != sequence
                || case.layout_only != previous_mode
            {
                history.clear();
                phrase.clear();
            }
            sequence = case.sequence;
            previous_mode = case.layout_only;
            let keys: Vec<_> = case
                .en
                .chars()
                .zip(case.he.chars())
                .map(|(en, he)| {
                    (
                        en.to_ascii_lowercase(),
                        he,
                        en.is_ascii_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(en),
                    )
                })
                .collect();
            let result = check_and_correct_in_context(
                &keys,
                |k| Some(k.0),
                |k| Some(k.1),
                |k| k.2,
                history.run(),
                en_dict(),
                he_dict(),
                Some(case.current),
                case.layout_only,
                context.then_some(&phrase),
                |_| crate::layout::LayoutSwitch::Switched,
            );
            if let Some(language) = result.lang {
                history.push(language);
            }
            let after = match result.fix {
                None => case.before().to_owned(),
                Some(Fix::Layout { start, text, .. }) => {
                    case.before().chars().take(start).collect::<String>() + &text
                }
                Some(
                    Fix::Spelling { text, .. }
                    | Fix::LayoutSpelling { text, .. }
                    | Fix::Suffix { text, .. },
                ) => text,
            };
            if case.layout_only {
                phrase.clear();
            } else if context {
                phrase.observe(&after, false);
            }
            Observation {
                after,
                abstain: result.abstain.map(|a| a.tag),
            }
        })
        .collect()
}

#[derive(Default, Debug, PartialEq)]
struct Metrics {
    correct: usize,
    unwanted: usize,
    wrong: usize,
    missed: usize,
    unchanged: usize,
    recovered: usize,
    introduced: usize,
    lost: usize,
}

impl Metrics {
    fn add(&mut self, before: &str, expected: &str, after: &str, baseline: &str) {
        if expected == before {
            if after == before {
                self.unchanged += 1;
            } else {
                self.unwanted += 1;
            }
        } else if after == expected {
            self.correct += 1;
        } else if after == before {
            self.missed += 1;
        } else {
            self.wrong += 1;
        }
        self.recovered +=
            usize::from(expected != before && baseline == before && after == expected);
        let bad = |text: &str| text != before && text != expected;
        self.introduced += usize::from(bad(after) && !bad(baseline));
        self.lost += usize::from(baseline == expected && after != expected);
    }

    fn precision(&self) -> String {
        ratio(self.correct, self.correct + self.unwanted + self.wrong)
    }
    fn recall(&self) -> String {
        ratio(self.correct, self.correct + self.missed + self.wrong)
    }
}

fn ratio(numerator: usize, denominator: usize) -> String {
    if denominator == 0 {
        "n/a".into()
    } else {
        format!("{:.2}", 100.0 * numerator as f64 / denominator as f64)
    }
}

#[test]
#[ignore = "offline report; run make calibrate"]
fn calibration_report() {
    assert!(!Config::global().personal_enabled && !Config::global().rule_stats_enabled);
    println!("# Synthetic corpus sensitivity report; not population accuracy or a tuning recommendation.");
    println!(
        "# One threshold family changes per replay. Scale multiplies its required score lead."
    );
    println!("# Context uses each replay's visible outputs; layout history remains enabled in both modes.");
    println!(
        "# Labels are desired outputs; known-error baselines and updates never replace labels."
    );
    println!("summary\tcorpus\tcontext\tgate\tscale\tlayout\tcorrect\tunwanted\twrong\tmissed\tunchanged\tprecision_pct\trecall_pct\trecovered\tintroduced\tlost");
    for (name, data) in [
        (
            "development",
            include_str!("../../tests/data/corrections.tsv"),
        ),
        (
            "evaluation",
            include_str!("../../tests/data/evaluation.tsv"),
        ),
    ] {
        let cases = parse(data).unwrap();
        for context in [false, true] {
            let baseline = replay(&cases, context);
            for (gate, scales) in [
                ("baseline", &[1.0][..]),
                ("english", &[0.5, 0.75, 1.25][..]),
                ("hebrew", &[0.5, 0.75, 1.25][..]),
                ("bilingual", &[0.5, 0.75, 1.25][..]),
                ("joint", &[0.5, 0.75, 1.25][..]),
            ] {
                for &scale in scales {
                    let results = with_policy(gate, scale, || replay(&cases, context));
                    if gate == "baseline" {
                        assert_eq!(results, baseline, "replay must be deterministic");
                    }
                    let mut abstentions = BTreeMap::<(&str, &str), usize>::new();
                    for (language, layout) in [
                        (None, "all"),
                        (Some(Language::English), "en"),
                        (Some(Language::Hebrew), "he"),
                    ] {
                        let mut metrics = Metrics::default();
                        for ((case, result), base) in cases.iter().zip(&results).zip(&baseline) {
                            if language.is_some_and(|lang| lang != case.current) {
                                continue;
                            }
                            metrics.add(case.before(), case.expected, &result.after, &base.after);
                            if result.after == case.before() {
                                *abstentions
                                    .entry((layout, result.abstain.unwrap_or("untagged_unchanged")))
                                    .or_default() += 1;
                            }
                        }
                        println!("summary\t{name}\t{context}\t{gate}\t{scale}\t{layout}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}", metrics.correct, metrics.unwanted, metrics.wrong, metrics.missed, metrics.unchanged, metrics.precision(), metrics.recall(), metrics.recovered, metrics.introduced, metrics.lost);
                    }
                    for ((layout, reason), count) in abstentions {
                        println!("abstention\t{name}\t{context}\t{gate}\t{scale}\t{layout}\t{reason}\t{count}");
                    }
                    for ((case, result), base) in cases.iter().zip(&results).zip(&baseline) {
                        if result.after != base.after {
                            println!("change\t{name}\t{context}\t{gate}\t{scale}\t{}\t{:?}\t{:?}\t{:?}\t{:?}\t{}", case.line, case.before(), case.expected, base.after, result.after, base.abstain.unwrap_or("untagged"));
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn metrics_cover_errors_and_zero_denominators() {
    let mut metrics = Metrics::default();
    assert_eq!(metrics.precision(), "n/a");
    assert_eq!(metrics.recall(), "n/a");
    metrics.add("name", "name", "word", "name");
    metrics.add("typo", "fix", "other", "typo");
    metrics.add("typo", "fix", "typo", "fix");
    metrics.add("typo", "fix", "fix", "typo");
    metrics.add("name", "name", "name", "name");
    assert_eq!(
        metrics,
        Metrics {
            correct: 1,
            unwanted: 1,
            wrong: 1,
            missed: 1,
            unchanged: 1,
            recovered: 1,
            introduced: 2,
            lost: 2
        }
    );
    assert_eq!(metrics.precision(), "33.33");
    assert_eq!(metrics.recall(), "33.33");
}

#[test]
fn corpus_validation_and_labels() {
    for invalid in [
        "",
        "en\tx\tx",
        "xx\tx\tx\tx",
        "en\tx\tשש\tx",
        "en\tx\tש\t",
        "en\tx\tש\tx\t\ts\tinvalid",
    ] {
        assert!(parse(invalid).is_err(), "{invalid:?}");
    }
    let cases = parse("# comment\nhe\tx\tש\tdesired\tknown-error\tseq\tfull\n").unwrap();
    assert_eq!(cases[0].expected, "desired");
    assert_eq!(cases[0].before(), "ש");
    assert_eq!(cases[0].line, 2);
}

#[test]
fn overrides_are_scoped_and_thread_local() {
    assert_eq!(threshold("english", 20.0), 20.0);
    with_policy("english", 0.5, || {
        assert_eq!(threshold("english", 20.0), 10.0);
        assert_eq!(threshold("hebrew", 20.0), 20.0);
        std::thread::spawn(|| assert_eq!(threshold("english", 20.0), 20.0))
            .join()
            .unwrap();
        let panic = std::panic::catch_unwind(|| with_policy("english", 0.75, || panic!("restore")));
        assert!(panic.is_err());
        assert_eq!(threshold("english", 20.0), 10.0);
    });
    assert_eq!(threshold("english", 20.0), 20.0);
}

#[test]
fn joint_sweep_replays_the_actual_acceptance_gate() {
    let decide = || {
        select_plan(
            vec![Proposal {
                plan: Plan::Spell {
                    text: "receive".into(),
                },
                lead: 3.0,
                context_scored: true,
            }],
            Reading::new("recieve", 7),
            Reading::new("רקבןקהק", "רקבןקהק".len()),
            Some(Language::English),
            None,
            &mut String::new(),
            None,
        )
    };
    assert!(decide().plan.is_none());
    assert!(with_policy("joint", 0.75, decide).plan.is_some());
    assert!(with_policy("english", 0.5, decide).plan.is_none());
    assert!(decide().plan.is_none());
}

#[test]
fn replay_preserves_case_layout_mode_and_sequence_boundaries() {
    let corpus = concat!(
        "en\thello\tיקךךם\thello\t\tchat\tfull\n",
        "en\trecieve\tרקבןקהק\treceive\t\tchat\tfull\n",
        "en\tNASA\tמשדש\tNASA\t\tchat\tfull\n",
        "en\trecieve\tרקבןקהק\trecieve\t\tchat\tlayout-only\n",
        "he\takuo\tשלום\tשלום\t\tnew\tfull\n",
    );
    let cases = parse(corpus).unwrap();
    for context in [false, true] {
        let results = replay(&cases, context);
        for (case, result) in cases.iter().zip(&results) {
            assert_eq!(result.after, case.expected, "line {}", case.line);
        }
        assert_eq!(results, replay(&cases, context));
    }
}
