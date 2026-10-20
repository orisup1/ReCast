//! Ignored microbenchmarks for the pure hot paths and live desktop focus queries.
//!
//! Run with `make bench`. They intentionally report measurements without
//! asserting wall-clock limits, which would be noisy on shared CI runners.

use std::hint::black_box;
use std::time::Instant;

use crate::dictionary::{en_dict, en_freq, he_dict, he_freq, Dict};

fn report(name: &str, iterations: usize, started: Instant) {
    let elapsed = started.elapsed();
    eprintln!(
        "{name}: {iterations} iterations in {:.3?} ({:.0} ns/op)",
        elapsed,
        elapsed.as_nanos() as f64 / iterations as f64,
    );
}

#[test]
#[ignore = "microbenchmark; run with `make bench`"]
fn benchmark_dictionary_lookup() {
    let (en, he) = (en_dict(), he_dict());
    let words = ["hello", "keyboard", "שלום", "zzzqqq", "correction"];
    let iterations = 500_000;
    let started = Instant::now();
    for i in 0..iterations {
        let word = black_box(words[i % words.len()]);
        black_box(en.contains(word));
        black_box(he.contains(word));
    }
    report("dictionary lookup pair", iterations, started);
}

#[test]
#[ignore = "microbenchmark; run with `make bench`"]
fn benchmark_dictionary_lookup_mixed() {
    // Sample both corpora outside the timed loop, rather than repeatedly
    // touching only the few cache-hot words in the smaller benchmark.
    let (en, he) = (en_dict(), he_dict());
    let en_blob = include_str!(concat!(env!("OUT_DIR"), "/en_dict.blob"));
    let he_blob = include_str!(concat!(env!("OUT_DIR"), "/he_dict.blob"));
    let words: Vec<_> = en_blob
        .lines()
        .step_by(97)
        .chain(he_blob.lines().step_by(97))
        .collect();
    let iterations = 500_000;
    for (name, en, he) in [
        (
            "dictionary lookup pair, mixed, unindexed",
            Dict::new(en_blob),
            Dict::new(he_blob),
        ),
        ("dictionary lookup pair, mixed, indexed", en, he),
    ] {
        let mut position = 0usize;
        let started = Instant::now();
        for _ in 0..iterations {
            position = (position + 7919) % words.len();
            let word = black_box(words[position]);
            black_box(en.contains(word));
            black_box(he.contains(word));
        }
        report(name, iterations, started);
    }
}

#[test]
#[ignore = "microbenchmark; run with `make bench`"]
fn benchmark_spelling_latency_distribution() {
    let (dict, freq) = (en_dict(), en_freq());
    let words = [
        "recieve",
        "keyboad",
        "restaraunt",
        "supino",
        "correct",
        "xomputer",
        "zeceive",
        "independance",
        "zzzzzzzzzzzzzzzzzzzzzzzz",
        "communication",
        "mispelling",
        "ropblem",
    ];
    let mut samples = Vec::with_capacity(1_000);
    for word in words {
        black_box(crate::spell::correct_with(word, dict, freq, 4, 50_000, 3));
    }
    for i in 0..1_000 {
        let started = Instant::now();
        black_box(crate::spell::correct_with(
            black_box(words[i % words.len()]),
            dict,
            freq,
            crate::config::DEFAULT_SPELL_MIN_LEN,
            crate::config::DEFAULT_SPELL_MAX_RANK,
            crate::config::DEFAULT_SPELL_MAX_DIST,
        ));
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    eprintln!(
        "spelling latency: p50 {:?}, p95 {:?}, max {:?} ({} samples)",
        samples[499],
        samples[949],
        samples[999],
        samples.len()
    );
}

#[test]
#[ignore = "microbenchmark; run with `make bench`"]
fn benchmark_spelling_correction() {
    let (dict, freq) = (en_dict(), en_freq());
    let words = ["recieve", "keyboad", "restaraunt", "supino", "correct"];
    let iterations = 5_000;
    let started = Instant::now();
    for i in 0..iterations {
        black_box(crate::spell::correct_with(
            black_box(words[i % words.len()]),
            dict,
            freq,
            crate::config::DEFAULT_SPELL_MIN_LEN,
            crate::config::DEFAULT_SPELL_MAX_RANK,
            crate::config::DEFAULT_SPELL_MAX_DIST,
        ));
    }
    report("spelling correction", iterations, started);
}

#[test]
#[ignore = "microbenchmark; run with `make bench`"]
fn benchmark_completion_candidates() {
    let prefixes = [
        "he",
        "hel",
        "keyb",
        "keybaot",
        "commnuic",
        "אוניבסוט",
        "corr",
        "comp",
        "zzz",
    ];
    let iterations = 20_000;
    let started = Instant::now();
    for i in 0..iterations {
        let prefix = prefixes[i % prefixes.len()];
        let (dict, freq) = if prefix.is_ascii() {
            (en_dict(), en_freq())
        } else {
            (he_dict(), he_freq())
        };
        black_box(crate::complete::completions_with(
            black_box(prefix),
            dict,
            freq,
            crate::config::DEFAULT_COMPLETE_MIN_LEN,
            crate::config::DEFAULT_COMPLETE_MAX_RANK,
        ));
    }
    report("completion candidates", iterations, started);
}

#[test]
#[ignore = "microbenchmark; run with `make bench`"]
fn benchmark_intent_latency_distribution() {
    use crate::complete::PhraseContext;
    use crate::dictionary::{he_freq, Freq};
    use std::time::Duration;

    const SAMPLES: usize = 200;

    struct Case {
        context: PhraseContext,
        words: &'static [&'static str],
        prefix: &'static str,
        typo: &'static str,
    }

    fn build_context(words: &[&str]) -> PhraseContext {
        let mut context = PhraseContext::default();
        for word in words {
            context.observe(word, false);
        }
        context
    }

    fn case(words: &'static [&'static str], prefix: &'static str, typo: &'static str) -> Case {
        Case {
            context: build_context(words),
            words,
            prefix,
            typo,
        }
    }

    fn distribution(name: &str, samples: &mut [Duration]) {
        samples.sort_unstable();
        eprintln!(
            "{name}: p50 {:?}, p95 {:?}, max {:?} ({} samples)",
            samples[(samples.len() - 1) * 50 / 100],
            samples[(samples.len() - 1) * 95 / 100],
            samples[samples.len() - 1],
            samples.len(),
        );
    }

    fn measure(language: &str, cases: &[Case], dict: Dict, freq: Freq, hebrew: bool) {
        let spelling = |case: &Case, context: Option<&PhraseContext>| {
            let word = black_box(case.typo);
            if hebrew {
                black_box(crate::spell::hebrew::assess(word, dict, freq, context));
            } else {
                black_box(crate::spell::assess(word, dict, freq, context));
            }
        };
        // Load lazy lists and exercise every case before measuring steady-state
        // requests. Context construction is outside the timed candidate paths.
        for case in cases {
            for context in [None, Some(&case.context)] {
                black_box(crate::complete::completions_in_context(
                    case.prefix,
                    dict,
                    freq,
                    context,
                ));
                spelling(case, context);
            }
        }

        let mut construction = Vec::with_capacity(SAMPLES);
        for sample in 0..SAMPLES {
            let case = &cases[sample % cases.len()];
            let started = Instant::now();
            let context = build_context(black_box(case.words));
            black_box(&context);
            construction.push(started.elapsed());
            // Destruction is excluded from the construction measurement.
            drop(context);
        }
        distribution(
            &format!("intent {language} context construction (full word sequence, not per key)"),
            &mut construction,
        );

        let mut completion = [Vec::with_capacity(SAMPLES), Vec::with_capacity(SAMPLES)];
        let mut automatic = [Vec::with_capacity(SAMPLES), Vec::with_capacity(SAMPLES)];
        for sample in 0..SAMPLES {
            let case = &cases[sample % cases.len()];
            // Alternate which mode runs first to reduce cache and temporal bias.
            for mode in [sample % 2, 1 - sample % 2] {
                let context = (mode == 1).then_some(&case.context);
                let started = Instant::now();
                black_box(crate::complete::completions_in_context(
                    black_box(case.prefix),
                    dict,
                    freq,
                    context,
                ));
                completion[mode].push(started.elapsed());

                let started = Instant::now();
                spelling(case, context);
                automatic[mode].push(started.elapsed());
            }
        }
        for (mode, label) in ["without context", "with context"].into_iter().enumerate() {
            distribution(
                &format!("intent {language} completion {label}"),
                &mut completion[mode],
            );
            distribution(
                &format!("intent {language} automatic spelling {label}"),
                &mut automatic[mode],
            );
        }
    }

    let english = [
        case(&["we", "go", "to", "the"], "ban", "keyboad"),
        case(&["we", "store", "all", "the", "data"], "bas", "baye"),
        case(&["i", "would", "like", "to", "thank"], "you", "yuo"),
        case(&["this", "is", "my"], "comp", "comqztxr"),
        case(&["please", "write", "the", "correct"], "spel", "speling"),
    ];
    let hebrew = [
        case(&["אני", "רוצה", "לומר", "לך", "תודה"], "רבה", "רבבה"),
        case(&["אני", "מקליד", "באמצעות"], "מקל", "מקלדתת"),
        case(&["אני", "עובד", "עכשיו", "עם"], "מחש", "מחששב"),
        case(&["אני", "רוצה", "לומר"], "שלו", "שלוום"),
        case(&["אני", "משתמש"], "אינ", "אינטררנטט"),
        case(&["אני", "מפתח"], "מער", "מעכרותת"),
    ];
    measure("English", &english, en_dict(), en_freq(), false);
    measure("Hebrew", &hebrew, he_dict(), he_freq(), true);
}

/// Read-only: queries focus without capturing keys, switching layouts or injecting.
#[test]
#[ignore = "requires a desktop session; run with make bench"]
fn benchmark_focus_queries() {
    use crate::platform::engine::Platform;
    #[cfg(target_os = "linux")]
    use crate::platform::linux::Linux as Native;
    #[cfg(target_os = "macos")]
    use crate::platform::macos::Mac as Native;
    #[cfg(target_os = "windows")]
    use crate::platform::windows::Windows as Native;

    let mut samples = Vec::with_capacity(100);
    let mut available = 0;
    for _ in 0..100 {
        let started = Instant::now();
        available += usize::from(black_box(Native::focus()).is_some());
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    eprintln!(
        "focus queries: {available}/100 available; p50 {:?}, p95 {:?}, max {:?}",
        samples[49], samples[94], samples[99]
    );
}
