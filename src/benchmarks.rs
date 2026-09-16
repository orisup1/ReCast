//! Ignored microbenchmarks for the pure hot paths and live desktop focus queries.
//!
//! Run with `make bench`. They intentionally report measurements without
//! asserting wall-clock limits, which would be noisy on shared CI runners.

use std::hint::black_box;
use std::time::Instant;

use crate::dictionary::{en_dict, en_freq, he_dict};

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
    let (dict, freq) = (en_dict(), en_freq());
    let prefixes = ["hel", "keyb", "corr", "comp", "zzz"];
    let iterations = 20_000;
    let started = Instant::now();
    for i in 0..iterations {
        black_box(crate::complete::completions_with(
            black_box(prefixes[i % prefixes.len()]),
            dict,
            freq,
            crate::config::DEFAULT_COMPLETE_MIN_LEN,
            crate::config::DEFAULT_COMPLETE_MAX_RANK,
        ));
    }
    report("completion candidates", iterations, started);
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
