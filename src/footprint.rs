//! What the process is actually costing, and the guarantee that it stays that
//! way.
//!
//! ReCast is a daemon: it starts at login and is expected to still be running
//! weeks later. That makes memory growth a different kind of bug from the usual
//! one — there is no natural end to the run that would hide it, so anything
//! that grows per keystroke or per correction eventually becomes the largest
//! thing on the user's machine that they never asked for.
//!
//! Three structures used to grow without limit and are now bounded at their
//! source: the word buffer ([`crate::types::WordBuffer`]), the session list of
//! undone words (`complete::MAX_SUPPRESSED`), and — already bounded before this
//! — the corrections history (`types::HISTORY_LEN`).
//!
//! What is left is essentially constant: the ~11 MB of dictionary and frequency
//! blobs, which are not heap at all. They are read-only pages of the executable
//! (see `dictionary.rs`), so they are shared, never copied, never freed and
//! reclaimable by the OS under pressure — the resident figure creeps up only as
//! binary searches touch more distinct pages, and stops at the size of the
//! data. That is why the ceiling below is a ceiling rather than a target.

/// Resident set size in bytes, if the OS will tell us cheaply.
///
/// Linux only. macOS and Windows both have an answer — `task_info` and
/// `GetProcessMemoryInfo` — but each costs an FFI surface and a dependency
/// feature for a number that is only ever displayed, and the daemon case this
/// exists for is the Linux one. `None` is reported as "unknown" rather than
/// guessed at.
pub fn rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // `VmRSS:  12345 kB` — read rather than /proc/self/statm so the units
        // are stated by the kernel instead of inferred from the page size.
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// The figure for a status line, e.g. `14.2 MB`.
pub fn rss_human() -> Option<String> {
    let bytes = rss_bytes()?;
    Some(format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0)))
}

/// The ceiling ReCast is expected to stay under, whatever it is asked to do.
///
/// Deliberately checked in a test rather than enforced at runtime: there is
/// nothing sensible for the program to *do* on hitting a memory limit, and the
/// point is that the limit is unreachable by design. A build that breaks this
/// has reintroduced unbounded growth somewhere, and the test is how that gets
/// noticed before a user's machine does.
///
/// Test-only for that reason: it is the contract the suite enforces, not a
/// runtime setting anything reads — and the test that enforces it needs
/// `VmRSS`, so it only exists where [`rss_bytes`] returns something.
#[cfg(all(test, target_os = "linux"))]
pub const CEILING_BYTES: u64 = 50 * 1024 * 1024;

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::dictionary::{en_dict, en_freq, he_dict};
    use crate::types::{AppControl, FixKind, WordBuffer};

    /// Work of the shape a long-lived daemon does: words checked against both
    /// dictionaries, completions scanned, corrections recorded, words undone,
    /// and a buffer fed keys that never end a word.
    fn a_lot_of_typing(control: &AppControl, rounds: usize) {
        let (en, he, freq) = (en_dict(), he_dict(), en_freq());
        for i in 0..rounds {
            // Spread across the alphabet rather than sitting in one region of
            // the blobs. The resident figure for the embedded dictionaries is
            // the count of *distinct pages a binary search has landed on*, so a
            // workload that only ever looks up `word…` touches a sliver of them
            // and would flatter the measurement badly.
            let a = (b'a' + (i % 26) as u8) as char;
            let b = (b'a' + ((i / 26) % 26) as u8) as char;
            let word = format!("{a}{b}{}", i % 977);
            // Dictionary work — this is what touches the embedded blobs.
            let _ = en.contains(&word);
            let _ = he.contains(&word);
            let _ = freq.rank(&word);
            let _ = crate::complete::completions_with(&word, en, freq, 3, 30_000);
            let _ = crate::spell::correct(&word, en, freq);
            // Per-correction state.
            control.record_fix(&word, "fixed", FixKind::Spelling);
            crate::complete::suppress(&word);
        }
    }

    #[test]
    fn a_long_session_does_not_grow_without_limit() {
        let control = AppControl::new_for_test();

        // Warm up: fault in the dictionary pages and every allocator arena
        // this workload is ever going to want, so the measurement below is of
        // *growth* and not of one-off startup cost.
        a_lot_of_typing(&control, 2_000);
        let settled = rss_bytes().expect("Linux reports VmRSS");

        // Then do far more of the same. Nothing here is a new kind of work —
        // it is the same session, continuing.
        a_lot_of_typing(&control, 20_000);
        let after = rss_bytes().expect("Linux reports VmRSS");

        let growth = after.saturating_sub(settled);
        // Captured by cargo unless the test fails; `--nocapture` shows the
        // figures, which is what makes this a measurement and not just a
        // tripwire.
        eprintln!(
            "settled {:.1} MB → {:.1} MB after 10× the work (+{:.2} MB), ceiling {} MB",
            settled as f64 / 1048576.0,
            after as f64 / 1048576.0,
            growth as f64 / 1048576.0,
            CEILING_BYTES / 1048576,
        );
        assert!(
            growth < 8 * 1024 * 1024,
            "ten times the work added {:.1} MB — something is accumulating \
             (settled at {:.1} MB, ended at {:.1} MB)",
            growth as f64 / 1048576.0,
            settled as f64 / 1048576.0,
            after as f64 / 1048576.0,
        );
        assert!(
            after < CEILING_BYTES,
            "{:.1} MB is over the {} MB ceiling",
            after as f64 / 1048576.0,
            CEILING_BYTES / 1048576,
        );
    }

    #[test]
    fn the_word_buffer_refuses_to_grow_into_a_token() {
        let mut buf: WordBuffer<u8> = WordBuffer::new();
        for _ in 0..100_000 {
            buf.push(b'a');
        }
        // Not merely capped — given up on, because a buffer holding some
        // arbitrary window of a 100,000-character token would have the
        // correction erase the wrong characters.
        assert!(buf.is_empty(), "an over-long run stops being a word");

        // And the next real word is unaffected.
        buf.clear();
        for c in b"hello" {
            buf.push(*c);
        }
        assert_eq!(&*buf, b"hello");
    }

    #[test]
    fn the_undone_word_list_is_capped() {
        for i in 0..5_000 {
            crate::complete::suppress(&format!("undone{i}"));
        }
        // The newest is still honoured...
        assert!(crate::complete::suppressed("undone4999"));
        // ...and the oldest has been let go rather than kept forever.
        assert!(!crate::complete::suppressed("undone0"));
    }
}
