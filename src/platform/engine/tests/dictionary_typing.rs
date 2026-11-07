//! Input-to-visible-output acceptance test, using the real shared typing engine.
//!
//! Run explicitly (this can take a long time):
//! cargo test --release dictionary_typing::dictionary_input_output -- --ignored --nocapture
//!
//! Optional: RECAST_DICTIONARY_SHARD=0/16 partitions dictionary rows into 16 runs.
//! Optional: RECAST_DICTIONARY_WORD=keyboard reproduces one exact dictionary entry.
//! No OS keys are sent. The existing simulated platform applies the engine's
//! actual erase/retype plans to a screen and checks their tracked suffixes.
//! Capture/injection races and application-specific OS behavior are not modeled.
//! Dictionary membership is a preservation hypothesis, not a human label:
//! reviewed misspellings in the dictionary may intentionally fail this audit.
//! Investigate failures; do not automatically relax production policy to pass.

use super::*;

const TEST: &str = "platform::engine::tests::dictionary_typing::dictionary_input_output";
const CHILD: &str = "RECAST_DICTIONARY_TYPING_CHILD";
const PREFIX: &str = "previous text: ";

#[derive(Default)]
struct Audit {
    entries: usize,
    unsupported: usize,
    cases: usize,
    failures: usize,
    recovered: usize,
    abstained: usize,
}

impl Audit {
    fn failure(&mut self, scenario: &str, word: &str, expected: &str, actual: &str) {
        self.failures += 1;
        // Bound output and memory even if a broken injector damages every row.
        if self.failures <= 100 {
            eprintln!(
                "FAIL scenario={scenario} word={word:?} expected={expected:?} actual={actual:?}"
            );
        }
    }
}

/// Translate through the platform's real key map, then verify the round trip.
/// Do not silently pass unsupported Unicode through as a synthetic physical key.
fn physical_keys(word: &str, language: Language) -> Option<String> {
    let keys = if language == Language::Hebrew {
        crate::keymap::convert_selection(word)
    } else {
        word.to_owned()
    };
    if !keys
        .chars()
        .all(|key| key.is_ascii_lowercase() || "'-,.;/[]`".contains(key))
    {
        return None;
    }
    let visible: Option<String> = keys
        .chars()
        .map(|key| {
            if language == Language::Hebrew {
                Simulated::hebrew_char(key)
            } else {
                Simulated::english_char_plain(key)
            }
        })
        .collect();
    (visible.as_deref() == Some(word)).then_some(keys)
}

fn session(language: Language) -> Session {
    SIMULATED_LAYOUT.store(usize::from(language == Language::Hebrew), Ordering::SeqCst);
    let session = Session::new();
    // An untracked preceding field prefix detects over-erasure without giving
    // the corrector a language hint or phrase context for the word under test.
    *session.engine.injector.text.lock().unwrap() = PREFIX.into();
    session
}

fn settle(session: &Session) {
    // Session typing waits for dictionary work. The scheduled replacement waits
    // at this gate until the captured input is on screen.
    if session.engine.lock().is_replacing {
        session.pending();
        session.finish();
    }
    assert!(!session.engine.lock().is_replacing);
    assert!(
        session.ready.try_recv().is_err(),
        "unexpected second injection"
    );
}

fn check_exact(audit: &mut Audit, scenario: &str, word: &str, expected: &str, session: &Session) {
    settle(session);
    audit.cases += 1;
    let actual = session.text();
    let expected = format!("{PREFIX}{expected}");
    if actual != expected {
        audit.failure(scenario, word, &expected, &actual);
    }
}

fn dictionary_entry(audit: &mut Audit, word: &str, language: Language) {
    audit.entries += 1;
    let Some(keys) = physical_keys(word, language) else {
        audit.unsupported += 1;
        if audit.unsupported <= 10 {
            eprintln!("UNSUPPORTED language={language:?} word={word:?}");
        }
        return;
    };
    for (scenario, ending) in [
        ("native-space", " "),
        ("native-newline", "\n"),
        ("native-punctuation", ". "),
    ] {
        let s = session(language);
        s.type_text(&keys);
        if language == Language::Hebrew && ending == ". " {
            // The English dot key types a Hebrew final letter. Send the key
            // whose Hebrew reading is actually a dot instead.
            let dot = (' '..='~')
                .find(|&key| Simulated::hebrew_char(key) == Some('.'))
                .expect("Hebrew layout must have a dot key");
            s.type_text(&format!("{dot} "));
        } else {
            s.type_text(ending);
        }
        check_exact(audit, scenario, word, &format!("{word}{ending}"), &s);
    }

    // Edit the unfinished word exactly as a user does, then commit it.
    let s = session(language);
    s.type_text(&keys);
    s.backspace();
    s.type_text(&keys.chars().last().unwrap().to_string());
    s.type_text(" ");
    check_exact(audit, "backspace-retype", word, &format!("{word} "), &s);

    let wrong_layout = language.other();
    let s = session(wrong_layout);
    s.type_text(&keys);
    let original = s.text();
    s.type_text(" ");
    settle(&s);
    audit.cases += 1;
    let actual = s.text();
    let desired = format!("{PREFIX}{word} ");
    if actual == desired {
        audit.recovered += 1;
    } else if actual == format!("{original} ") {
        // A dictionary entry alone cannot establish intent in an ambiguous
        // layout. Abstention is a recall measurement, not an injection defect.
        audit.abstained += 1;
    } else {
        audit.failure("wrong-layout", word, &desired, &actual);
    }
}

#[test]
#[ignore = "exhaustive typing-engine audit; run explicitly in release mode"]
fn dictionary_input_output() {
    // The simulator, configuration and learned suppressions are process-global.
    // A child avoids changing any other test, even with a parallel test runner.
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                TEST,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .env_remove("RECAST_PREVIEW_TEST")
            .env("RECAST_DEBUG", "0")
            .status()
            .expect("start isolated dictionary typing audit");
        assert!(
            status.success(),
            "dictionary typing audit failed; see child output"
        );
        return;
    }
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    let config = crate::config::Config::global();
    assert!(!config.personal_enabled && !config.rule_stats_enabled);
    let shard = std::env::var("RECAST_DICTIONARY_SHARD").unwrap_or_else(|_| "0/1".into());
    let (index, count) = shard
        .split_once('/')
        .expect("shard must be index/count, e.g. 0/16");
    let (index, count): (usize, usize) = (index.parse().unwrap(), count.parse().unwrap());
    assert!(
        count > 0 && index < count,
        "shard index must be below positive count"
    );
    let filter = std::env::var("RECAST_DICTIONARY_WORD").ok();
    let mut audit = Audit::default();

    // Human-labeled examples ensure an engine that never corrects cannot pass
    // merely by preserving dictionary words. Expectations do not call a planner.
    for (keys, language, expected) in [
        ("recieve", Language::English, "receive "),
        ("akuo", Language::English, "שלום "),
        ("hello", Language::Hebrew, "hello "),
        ("user_id", Language::English, "user_id "),
        ("github", Language::English, "github "),
    ] {
        let s = session(language);
        s.type_text(keys);
        s.type_text(" ");
        check_exact(&mut audit, "labeled-control", keys, expected, &s);
    }

    // Iterate the exact built dictionaries used by the app, not frequency-list
    // approximations. Stable row numbering makes shards reproducible per build.
    let mut row = 0usize;
    for (language, dict) in [
        (Language::English, crate::dictionary::en_dict()),
        (Language::Hebrew, crate::dictionary::he_dict()),
    ] {
        dict.for_each_with_prefix("", |word| {
            let selected =
                row % count == index && filter.as_ref().is_none_or(|filter| filter == word);
            row += 1;
            if selected {
                dictionary_entry(&mut audit, word, language);
                if audit.entries % 10_000 == 0 {
                    eprintln!(
                        "progress entries={} cases={} failures={}",
                        audit.entries, audit.cases, audit.failures
                    );
                }
            }
        });
    }
    eprintln!("dictionary typing: shard={shard} entries={} unsupported={} cases={} recovered={} abstained={} failures={} (first 100 failures shown)",
        audit.entries, audit.unsupported, audit.cases, audit.recovered, audit.abstained, audit.failures);
    assert!(
        audit.entries > audit.unsupported,
        "no representable dictionary words selected"
    );
    assert_eq!(
        audit.failures, 0,
        "unexpected visible outputs; review dictionary hypotheses and labeled controls"
    );
}
