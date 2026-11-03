//! Passive intelligence: personal frequency, confusion pairs, typing patterns.
//!
//! All three are built by watching what the user actually does — accepted fixes,
//! undo gestures, completions taken, and raw timing — and written to files in
//! the config directory so they survive restarts. This is deliberately opt-in:
//! word-frequency and confusion files can contain sensitive text even though
//! they never leave the machine.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::complete::config_dir;
use crate::config::Config;

/// Where personal data lives.
const PERSONAL_DIR: &str = "personal";

/// Personal frequency file: `word<TAB>count` per line, most frequent first.
const PERSONAL_FREQ_FILE: &str = "freq.txt";

/// Confusion pairs file: `typed<TAB>corrected<TAB>count<TAB>decayed_at` per line.
const CONFUSIONS_FILE: &str = "confusions.txt";

/// Typing pattern profile: aggregated statistics, JSON-ish text for readability.
const PROFILE_FILE: &str = "profile.txt";
const RULE_STATS_FILE: &str = "rules.txt";
const RULES: [&str; 5] = [
    "layout",
    "split",
    "spelling",
    "abbreviation",
    "layout+spelling",
];

const USEFULNESS_FILE: &str = "usefulness.txt";
const USE_NAMES: [&str; 7] = [
    "completion_sessions",
    "completion_cycle_taps",
    "completion_accepted",
    "completion_first_choice",
    "completion_abandoned",
    "automatic_corrections",
    "correction_undos",
];
static USE_DIRTY: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
pub(crate) enum UseEvent {
    Session,
    CycleTap,
    Accepted,
    FirstChoice,
    Abandoned,
    Correction,
    Undo,
}

fn usefulness() -> &'static Mutex<[u64; 7]> {
    static COUNTS: OnceLock<Mutex<[u64; 7]>> = OnceLock::new();
    COUNTS.get_or_init(|| {
        let text = personal_path(USEFULNESS_FILE)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        Mutex::new(parse_usefulness(&text))
    })
}

fn parse_usefulness(text: &str) -> [u64; 7] {
    let mut counts = [0; 7];
    for line in text.lines() {
        if let Some((name, count)) = line.split_once('\t') {
            if let (Some(index), Ok(count)) = (
                USE_NAMES.iter().position(|known| *known == name),
                count.parse(),
            ) {
                counts[index] = count;
            }
        }
    }
    counts
}

pub(crate) fn record_use(event: UseEvent) {
    if !Config::global().rule_stats_enabled {
        return;
    }
    if let Ok(mut counts) = usefulness().lock() {
        increment(&mut counts[event as usize]);
        USE_DIRTY.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
pub(crate) fn usefulness_counts() -> [u64; 7] {
    *usefulness().lock().unwrap()
}

pub(crate) fn usefulness_summary() -> String {
    let counts = usefulness()
        .lock()
        .map(|counts| *counts)
        .unwrap_or_default();
    format!("{} accepted / {} offers, {} first choice, {} cycle taps, {} abandoned; {} correction undos / {} corrections",
        counts[2], counts[0], counts[3], counts[1], counts[4], counts[6], counts[5])
}

fn flush_usefulness() -> std::io::Result<()> {
    let Some(path) = personal_path(USEFULNESS_FILE) else {
        return Ok(());
    };
    let counts = *usefulness()
        .lock()
        .map_err(|_| std::io::Error::other("usefulness statistics lock poisoned"))?;
    let mut text =
        String::from("# Aggregate usefulness counts; no typed text or application identities.\n");
    for (name, count) in USE_NAMES.iter().zip(counts) {
        text.push_str(&format!("{name}\t{count}\n"));
    }
    write_private(&path, &text)
}

/// Max entries kept in each file. Kept bounded so a long-running daemon
/// doesn't accumulate unbounded memory/disk.
const MAX_PERSONAL_ENTRIES: usize = 5000;

/// How often to flush dirty state to disk (seconds).
const FLUSH_INTERVAL: Duration = Duration::from_secs(30);

/// Minimum word length to enter personal frequency (filters single letters, etc.).
const MIN_WORD_LEN: usize = 3;

static FREQ_DIRTY: AtomicBool = AtomicBool::new(false);
static CONFUSIONS_DIRTY: AtomicBool = AtomicBool::new(false);
static PROFILE_DIRTY: AtomicBool = AtomicBool::new(false);
static RULE_STATS_DIRTY: AtomicBool = AtomicBool::new(false);

fn increment(count: &mut u64) {
    *count = count.saturating_add(1);
}

fn rule_stats() -> &'static Mutex<[[u64; 2]; RULES.len()]> {
    static STATS: OnceLock<Mutex<[[u64; 2]; RULES.len()]>> = OnceLock::new();
    STATS.get_or_init(|| {
        let saved = personal_path(RULE_STATS_FILE)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        Mutex::new(parse_rule_stats(&saved))
    })
}

fn parse_rule_stats(text: &str) -> [[u64; 2]; RULES.len()] {
    let mut counts = [[0; 2]; RULES.len()];
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        let mut fields = line.split('\t');
        let (Some(tag), Some(applied), Some(undone), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if let (Some(index), Ok(applied), Ok(undone)) = (
            RULES.iter().position(|known| *known == tag),
            applied.parse(),
            undone.parse(),
        ) {
            counts[index] = [applied, undone];
        }
    }
    counts
}

fn rule_stats_text(counts: [[u64; 2]; RULES.len()]) -> String {
    let mut out = String::from("# rule\tautomatic corrections\tundos\n");
    for (tag, [applied, undone]) in RULES.iter().zip(counts) {
        out.push_str(&format!("{tag}\t{applied}\t{undone}\n"));
    }
    out
}

/// Count a completed automatic correction or its undo, without its text.
pub fn record_rule(tag: &str, undone: bool) {
    if !Config::global().rule_stats_enabled {
        return;
    }
    let Some(index) = RULES.iter().position(|known| *known == tag) else {
        return;
    };
    record_use(if undone {
        UseEvent::Undo
    } else {
        UseEvent::Correction
    });
    if let Ok(mut counts) = rule_stats().lock() {
        increment(&mut counts[index][usize::from(undone)]);
        RULE_STATS_DIRTY.store(true, Ordering::Relaxed);
    }
}

fn flush_rule_stats() -> std::io::Result<()> {
    let Some(path) = personal_path(RULE_STATS_FILE) else {
        return Ok(());
    };
    let counts = *rule_stats()
        .lock()
        .map_err(|_| std::io::Error::other("rule statistics lock poisoned"))?;
    let out = rule_stats_text(counts);
    write_private(&path, &out)
}

// ─────────────────────────────────────────────────────────────────────────────
// Personal frequency
// ─────────────────────────────────────────────────────────────────────────────

/// In-memory personal frequency map.
fn personal_freq_map() -> &'static Mutex<HashMap<String, u64>> {
    static MAP: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(load_personal_freq()))
}

/// Load the personal frequency file, or return empty map.
fn load_personal_freq() -> HashMap<String, u64> {
    let Some(path) = personal_path(PERSONAL_FREQ_FILE) else {
        return HashMap::new();
    };
    std::fs::read_to_string(&path)
        .ok()
        .as_deref()
        .map(parse_freq_file)
        .unwrap_or_default()
}

/// Parse `word<TAB>count` lines.
fn parse_freq_file(text: &str) -> HashMap<String, u64> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((word, count)) = line.split_once('\t') {
            if let Ok(count) = count.trim().parse::<u64>() {
                let word = word.trim().to_lowercase();
                if !word.is_empty() && (map.len() < MAX_PERSONAL_ENTRIES || map.contains_key(&word))
                {
                    map.insert(word, count);
                }
            }
        }
    }
    map
}

/// Record that `word` was typed/accepted. Call for every finished word that
/// passes basic sanity (length, ascii/hebrew). The map is flushed periodically.
pub fn record_word(word: &str) {
    if !enabled() {
        return;
    }
    let word = word.trim().to_lowercase();
    if word.chars().count() < MIN_WORD_LEN {
        return;
    }
    if let Ok(mut map) = personal_freq_map().lock() {
        // Keep the first 5,000 unique words; evict the least common
        // only if tail learning proves more useful than a hard memory bound.
        if map.len() >= MAX_PERSONAL_ENTRIES && !map.contains_key(&word) {
            return;
        }
        increment(map.entry(word).or_insert(0));
        FREQ_DIRTY.store(true, Ordering::Relaxed);
    }
}

/// Get how many times `word` has been observed locally.
pub fn personal_count(word: &str) -> Option<u64> {
    if !enabled() {
        return None;
    }
    let word = word.trim().to_lowercase();
    personal_freq_map().lock().ok()?.get(&word).copied()
}

/// Repeatedly retained words can be completed even when absent from the dictionary.
pub fn completion_words() -> Vec<String> {
    if !enabled() {
        return Vec::new();
    }
    personal_freq_map()
        .lock()
        .map(|map| {
            map.iter()
                .filter(|(_, count)| **count >= 2)
                .map(|(word, _)| word.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Boost a candidate's score in completions/spelling based on personal frequency.
/// Returns a multiplier (>= 1.0) applied to the candidate's value.
pub fn personal_boost(word: &str) -> f32 {
    let count = match personal_count(word) {
        Some(count) => count,
        None => return 1.0,
    };
    boost_for_count(count)
}

/// A bounded, monotonic boost: one observation is a hint; ten are the cap.
fn boost_for_count(count: u64) -> f32 {
    1.0 + (count as f32 / 10.0).min(1.0)
}

/// Write personal frequency to disk atomically.
fn flush_personal_freq() -> std::io::Result<()> {
    let Some(path) = personal_path(PERSONAL_FREQ_FILE) else {
        return Ok(());
    };
    let map = match personal_freq_map().lock() {
        Ok(m) => m.clone(),
        Err(_) => return Err(std::io::Error::other("personal frequency lock poisoned")),
    };
    let mut entries: Vec<_> = map.iter().collect();
    entries.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    entries.truncate(MAX_PERSONAL_ENTRIES);
    let mut out = String::from("# Personal word frequency — written by ReCast, safe to edit\n");
    for (word, count) in entries {
        out.push_str(word);
        out.push('\t');
        out.push_str(&count.to_string());
        out.push('\n');
    }
    write_private(&path, &out)
}

// ─────────────────────────────────────────────────────────────────────────────
// Confusion pairs (typed -> corrected)
// ─────────────────────────────────────────────────────────────────────────────

/// Retained votes halve every 30 days, independently of new observations.
const CONFUSION_HALF_LIFE: u64 = 30 * 24 * 60 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Confusion {
    count: u64,
    decayed_at: u64,
}

impl Confusion {
    fn count_at(self, now: u64) -> u64 {
        let periods = now.saturating_sub(self.decayed_at) / CONFUSION_HALF_LIFE;
        self.count.checked_shr(periods.min(64) as u32).unwrap_or(0)
    }

    fn decay(&mut self, now: u64) {
        self.count = self.count_at(now);
        let periods = now.saturating_sub(self.decayed_at) / CONFUSION_HALF_LIFE;
        self.decayed_at += periods * CONFUSION_HALF_LIFE;
    }
}

type Confusions = HashMap<String, HashMap<String, Confusion>>;

fn confusion_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// In-memory confusion map: what the user typed -> what it was corrected to.
fn confusions_map() -> &'static Mutex<Confusions> {
    static MAP: OnceLock<Mutex<Confusions>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(load_confusions()))
}

/// Load confusions file.
fn load_confusions() -> Confusions {
    let Some(path) = personal_path(CONFUSIONS_FILE) else {
        return HashMap::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    let map = parse_confusions_file(&text);
    // Save timestamps for legacy entries so their age survives the next restart.
    if !map.is_empty() {
        CONFUSIONS_DIRTY.store(true, Ordering::Relaxed);
    }
    map
}

/// Legacy three-column entries start aging when first loaded by this version.
fn parse_confusions_file(text: &str) -> Confusions {
    parse_confusions_at(text, confusion_time())
}

fn parse_confusions_at(text: &str, now: u64) -> Confusions {
    let mut outer: Confusions = HashMap::new();
    let mut entries = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if matches!(parts.len(), 3 | 4) {
            if let Ok(count) = parts[2].trim().parse::<u64>() {
                let decayed_at = match parts.get(3) {
                    Some(timestamp) => match timestamp.trim().parse::<u64>() {
                        Ok(timestamp) => timestamp.min(now),
                        Err(_) => continue,
                    },
                    None => now,
                };
                let typed = parts[0].trim().to_lowercase();
                let corrected = parts[1].trim().to_lowercase();
                if !typed.is_empty() && !corrected.is_empty() {
                    let known = outer
                        .get(&typed)
                        .is_some_and(|corrections| corrections.contains_key(&corrected));
                    if known || entries < MAX_PERSONAL_ENTRIES {
                        let corrections = outer.entry(typed).or_default();
                        if corrections
                            .insert(corrected, Confusion { count, decayed_at })
                            .is_none()
                        {
                            entries += 1;
                        }
                    }
                }
            }
        }
    }
    outer
}

/// Record one vote for an automatic correction retained during later typing.
pub fn record_confusion(typed: &str, corrected: &str) {
    record_confusion_with_weight(typed, corrected, 1);
}

/// An explicitly selected correction provides stronger evidence than retention.
pub(crate) fn record_explicit_confusion(typed: &str, corrected: &str) {
    record_confusion_with_weight(typed, corrected, 3);
}

fn record_confusion_with_weight(typed: &str, corrected: &str, weight: u64) {
    if !enabled() {
        return;
    }
    let typed = typed.trim().to_lowercase();
    let corrected = corrected.trim().to_lowercase();
    if typed.is_empty() || corrected.is_empty() || typed == corrected {
        return;
    }
    if let Ok(mut map) = confusions_map().lock() {
        let known = map
            .get(&typed)
            .is_some_and(|corrections| corrections.contains_key(&corrected));
        // This O(n) count runs only for a new correction pair; keep a
        // separate counter if adding pairs at the 5,000-entry ceiling is hot.
        if !known && map.values().map(HashMap::len).sum::<usize>() >= MAX_PERSONAL_ENTRIES {
            return;
        }
        let now = confusion_time();
        let evidence = map
            .entry(typed)
            .or_default()
            .entry(corrected)
            .or_insert(Confusion {
                count: 0,
                decayed_at: now,
            });
        evidence.decay(now);
        evidence.count = evidence.count.saturating_add(weight);
        CONFUSIONS_DIRTY.store(true, Ordering::Relaxed);
    }
}

/// A snapshot keeps locks and file reads out of the spelling candidate loop.
/// Repeated passive votes or a stronger explicit selection contribute evidence.
pub(crate) fn typo_counts() -> [u64; 3] {
    typo_counts_with(crate::spell::typo_class)
}

/// Keep Hebrew and English channel evidence separate; their letter and
/// repetition patterns are not interchangeable despite shared key geometry.
pub(crate) fn hebrew_typo_counts() -> [u64; 3] {
    typo_counts_with(crate::spell::hebrew::typo_class)
}

fn typo_counts_with(classify: fn(&str, &str) -> Option<usize>) -> [u64; 3] {
    let mut counts = [0u64; 3];
    if !enabled() {
        return counts;
    }
    if let Ok(map) = confusions_map().lock() {
        let now = confusion_time();
        for (typed, corrections) in map.iter() {
            for (corrected, evidence) in corrections {
                let count = evidence.count_at(now);
                if count >= 2 {
                    if let Some(class) = classify(typed, corrected) {
                        counts[class] = counts[class].saturating_add(count);
                    }
                }
            }
        }
    }
    counts
}

/// Undo withdraws three votes for this replacement, outweighing passive retention.
/// Deriving typo weights from these counts also weakens the rejected edit class.
pub(crate) fn reject_confusion(typed: &str, corrected: &str) {
    if !enabled() {
        return;
    }
    if let Ok(mut map) = confusions_map().lock() {
        if let Some(evidence) = map
            .get_mut(&typed.to_lowercase())
            .and_then(|inner| inner.get_mut(&corrected.to_lowercase()))
        {
            evidence.decay(confusion_time());
            evidence.count = evidence.count.saturating_sub(3);
            CONFUSIONS_DIRTY.store(true, Ordering::Relaxed);
        }
    }
}

/// Look up the most common correction for `typed` from personal confusions.
/// Require two votes, twice all competing votes, and a lead of at least two.
#[cfg(test)]
pub fn personal_correction(typed: &str) -> Option<String> {
    personal_candidate(typed).map(|(word, _)| word)
}

/// Carry the decayed support advantage into the joint decision rather than
/// giving a barely eligible pair the same weight as a well-established one.
pub(crate) fn personal_candidate(typed: &str) -> Option<(String, f32)> {
    if !enabled() {
        return None;
    }
    let typed = typed.trim().to_lowercase();
    let map = confusions_map().lock().ok()?;
    let inner = map.get(&typed)?;
    let now = confusion_time();
    let best = preferred_confusion(inner, now)?;
    let count = inner.get(best)?.count_at(now);
    let competing = inner
        .iter()
        .filter(|(word, _)| *word != best)
        .fold(0u64, |total, (_, evidence)| {
            total.saturating_add(evidence.count_at(now))
        });
    let lead = (4.0 + 2.0 * (count.saturating_sub(competing) as f32).ln()).min(12.0);
    Some((best.clone(), lead))
}

fn preferred_confusion(inner: &HashMap<String, Confusion>, now: u64) -> Option<&String> {
    let (best, count) = inner
        .iter()
        .map(|(word, evidence)| (word, evidence.count_at(now)))
        .max_by_key(|(_, count)| *count)?;
    let competing = inner
        .iter()
        .filter(|(word, _)| *word != best)
        .fold(0u64, |sum, (_, evidence)| {
            sum.saturating_add(evidence.count_at(now))
        });
    (count >= 2 && count / 2 >= competing && count.saturating_sub(competing) >= 2).then_some(best)
}

/// Write confusions to disk atomically.
fn flush_confusions() -> std::io::Result<()> {
    let Some(path) = personal_path(CONFUSIONS_FILE) else {
        return Ok(());
    };
    // Clone the map while holding the lock, then release it before writing.
    let map = match confusions_map().lock() {
        Ok(m) => m.clone(),
        Err(_) => return Err(std::io::Error::other("personal confusions lock poisoned")),
    };
    let out = confusions_text(&map);
    write_private(&path, &out)
}

fn confusions_text(map: &Confusions) -> String {
    let mut total_entries = 0;
    let mut out = String::from(
        "# Personal confusion pairs: typed\\tcorrected\\tcount\\tdecayed_at (Unix seconds)\n",
    );
    for (typed, inner) in map {
        if total_entries >= MAX_PERSONAL_ENTRIES {
            break;
        }
        let mut entries: Vec<_> = inner.iter().collect();
        entries.sort_by(|a, b| b.1.count.cmp(&a.1.count).then_with(|| a.0.cmp(b.0)));
        for (corrected, evidence) in entries {
            if total_entries >= MAX_PERSONAL_ENTRIES {
                break;
            }
            out.push_str(typed);
            out.push('\t');
            out.push_str(corrected);
            out.push('\t');
            out.push_str(&evidence.count.to_string());
            out.push('\t');
            out.push_str(&evidence.decayed_at.to_string());
            out.push('\n');
            total_entries += 1;
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Typing pattern profile (dwell times, digraph latencies)
// ─────────────────────────────────────────────────────────────────────────────

/// In-memory typing profile: aggregated dwell times and digraph intervals.
#[derive(Clone, Default)]
struct TypingProfile {
    /// Key -> list of dwell times (press to release) in microseconds.
    dwells: HashMap<String, VecDeque<u64>>,
    /// Digraph (prev_key, key) -> list of intervals in microseconds.
    digraphs: HashMap<(String, String), VecDeque<u64>>,
    /// Total keystrokes seen.
    total_keys: u64,
    /// Last key press time for digraph calculation.
    last_press: Option<(String, Instant)>,
    /// Press time by held key, for dwell calculation on release.
    presses: HashMap<String, Instant>,
}

fn typing_profile() -> &'static Mutex<TypingProfile> {
    static PROFILE: OnceLock<Mutex<TypingProfile>> = OnceLock::new();
    PROFILE.get_or_init(|| Mutex::new(TypingProfile::default()))
}

/// Record a key press for digraph timing.
pub fn record_key_press(key_name: &str) {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    let key = key_name.to_lowercase();
    if let Ok(mut profile) = typing_profile().lock() {
        if let Some((prev_key, prev_time)) = profile.last_press.take() {
            let interval = now.saturating_duration_since(prev_time).as_micros() as u64;
            if interval < 1_000_000 {
                // Cap at 1 second to filter pauses
                let digraph_key = (prev_key.clone(), key.clone());
                profile
                    .digraphs
                    .entry(digraph_key.clone())
                    .or_default()
                    .push_back(interval);
                // Keep only recent N per digraph
                if let Some(dq) = profile.digraphs.get_mut(&digraph_key) {
                    while dq.len() > 100 {
                        dq.pop_front();
                    }
                }
            }
        }
        profile.last_press = Some((key.clone(), now));
        profile.presses.insert(key, now);
        profile.total_keys += 1;
        PROFILE_DIRTY.store(true, Ordering::Relaxed);
    }
}

/// Record a key release for dwell time.
pub fn record_key_release(key_name: &str) {
    if !enabled() {
        return;
    }
    let key = key_name.to_lowercase();
    if let Ok(mut profile) = typing_profile().lock() {
        let Some(press_time) = profile.presses.remove(&key) else {
            return;
        };
        let dwell = Instant::now()
            .saturating_duration_since(press_time)
            .as_micros() as u64;
        if dwell > 1_000_000 {
            return;
        }
        profile
            .dwells
            .entry(key.clone())
            .or_default()
            .push_back(dwell);
        if let Some(dq) = profile.dwells.get_mut(&key) {
            while dq.len() > 100 {
                dq.pop_front();
            }
        }
        PROFILE_DIRTY.store(true, Ordering::Relaxed);
    }
}

/// Flush typing profile to disk (JSON-ish text).
fn flush_profile() -> std::io::Result<()> {
    let Some(path) = personal_path(PROFILE_FILE) else {
        return Ok(());
    };
    let profile = match typing_profile().lock() {
        Ok(p) => p.clone(),
        Err(_) => return Err(std::io::Error::other("typing profile lock poisoned")),
    };
    let mut out = String::from("# Typing profile — written by ReCast\n");
    out.push_str(&format!("total_keys: {}\n", profile.total_keys));
    let mut intervals: Vec<u64> = profile
        .digraphs
        .values()
        .flat_map(|values| values.iter().copied())
        .collect();
    intervals.sort_unstable();
    if let Some(global) = intervals.get(intervals.len() / 2) {
        out.push_str(&format!("global_median_interval_us: {}\n", global));
    }
    out.push_str("\n# Per-key median dwell (us)\n");
    for (key, dq) in &profile.dwells {
        if !dq.is_empty() {
            let mut v: Vec<u64> = dq.iter().copied().collect();
            v.sort_unstable();
            out.push_str(&format!("{}: {}\n", key, v[v.len() / 2]));
        }
    }
    out.push_str("\n# Per-digraph median interval (us)\n");
    for ((prev, key), dq) in &profile.digraphs {
        if !dq.is_empty() {
            let mut v: Vec<u64> = dq.iter().copied().collect();
            v.sort_unstable();
            out.push_str(&format!("{} {}: {}\n", prev, key, v[v.len() / 2]));
        }
    }
    write_private(&path, &out)
}

/// Only the background writer calls this. Clear before snapshotting so changes
/// arriving during a save remain dirty; failed writes are retried next time.
fn flush_if_dirty(dirty: &AtomicBool, flush: impl FnOnce() -> std::io::Result<()>) {
    if dirty.swap(false, Ordering::Relaxed) && flush().is_err() {
        dirty.store(true, Ordering::Relaxed);
    }
}

/// Periodic flush of all personal data.
fn spawn_periodic_flusher() {
    std::thread::Builder::new()
        .name("recast-personal-flush".into())
        .spawn(|| loop {
            std::thread::sleep(FLUSH_INTERVAL);
            flush_if_dirty(&FREQ_DIRTY, flush_personal_freq);
            flush_if_dirty(&CONFUSIONS_DIRTY, flush_confusions);
            flush_if_dirty(&PROFILE_DIRTY, flush_profile);
            flush_if_dirty(&RULE_STATS_DIRTY, flush_rule_stats);
            flush_if_dirty(&USE_DIRTY, flush_usefulness);
        })
        .ok();
}

/// Initialize whichever opt-in local data stores are enabled.
pub fn init() {
    if !enabled() && !Config::global().rule_stats_enabled {
        return;
    }
    let Some(dir) = data_dir() else {
        return;
    };
    if create_private_dir(&dir).is_err() {
        return;
    }
    static START: std::sync::Once = std::sync::Once::new();
    START.call_once(|| {
        if enabled() {
            personal_freq_map();
            confusions_map();
        }
        if Config::global().rule_stats_enabled {
            rule_stats();
            usefulness();
        }
        spawn_periodic_flusher();
    });
}

fn enabled() -> bool {
    Config::global().personal_enabled
}

/// Directory containing opt-in personal data, if this OS provides one.
pub fn data_dir() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(PERSONAL_DIR))
}

/// Delete only files ReCast owns after all other instances have stopped.
/// A live instance retains its loaded maps and could write deleted data back.
/// The directory removal is non-recursive, so unexpected files are preserved.
pub fn clear_data() -> Result<Option<PathBuf>, String> {
    if !crate::instance::running_pids().is_empty() {
        return Err("Stop ReCast first (quit it from the tray or stop its login service), then rerun --clear-personal-data. A running instance can restore deleted data from memory.".into());
    }
    let Some(dir) = data_dir() else {
        return Ok(None);
    };
    clear_dir(&dir)?;
    Ok(Some(dir))
}

fn clear_dir(dir: &std::path::Path) -> Result<(), String> {
    let names = [
        PERSONAL_FREQ_FILE,
        CONFUSIONS_FILE,
        PROFILE_FILE,
        RULE_STATS_FILE,
        USEFULNESS_FILE,
    ];
    for name in names {
        let path = dir.join(name);
        // Interrupted saves can leave the same private data in their snapshot.
        for path in [path.clone(), path.with_extension("txt.tmp")] {
            if let Err(error) = std::fs::remove_file(&path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(format!("{}: {error}", path.display()));
                }
            }
        }
    }
    // The atomic writer includes its PID in snapshot names. A crashed process
    // can leave those behind, so clear snapshots from previous runs as well.
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("{}: {error}", dir.display())),
    };
    for entry in entries.into_iter().flatten() {
        let entry = entry.map_err(|error| format!("{}: {error}", dir.display()))?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some((stem, pid)) = name
            .strip_suffix(".tmp")
            .and_then(|name| name.rsplit_once('.'))
        else {
            continue;
        };
        let owned_snapshot = pid.bytes().all(|c| c.is_ascii_digit())
            && pid.parse::<u32>().is_ok_and(|pid| pid > 0)
            && names
                .iter()
                .any(|name| name.strip_suffix(".txt") == Some(stem));
        if owned_snapshot {
            let path = entry.path();
            if let Err(error) = std::fs::remove_file(&path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(format!("{}: {error}", path.display()));
                }
            }
        }
    }
    match std::fs::remove_dir(dir) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(error) => return Err(format!("{}: {error}", dir.display())),
    }
    Ok(())
}

fn personal_path(name: &str) -> Option<PathBuf> {
    data_dir().map(|dir| dir.join(name))
}

fn create_private_dir(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_private(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    crate::settings::write_atomic(path, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn personal_saves_preserve_unrelated_symlink_targets() {
        const CHILD: &str = "RECAST_PERSONAL_SYMLINK_TEST";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "personal::tests::personal_saves_preserve_unrelated_symlink_targets",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let dir = data_dir().unwrap();
        create_private_dir(&dir).unwrap();
        for (name, flush) in [
            (
                PERSONAL_FREQ_FILE,
                flush_personal_freq as fn() -> std::io::Result<()>,
            ),
            (CONFUSIONS_FILE, flush_confusions),
            (PROFILE_FILE, flush_profile),
            (RULE_STATS_FILE, flush_rule_stats),
            (USEFULNESS_FILE, flush_usefulness),
        ] {
            let path = dir.join(name);
            let unrelated = dir.join(format!("unrelated-{name}"));
            std::fs::write(&unrelated, "keep this file\n").unwrap();
            let old_temp = path.with_extension("txt.tmp");
            std::os::unix::fs::symlink(&unrelated, &old_temp).unwrap();
            flush().unwrap();
            assert_eq!(
                std::fs::read_to_string(&unrelated).unwrap(),
                "keep this file\n"
            );
            assert!(std::fs::symlink_metadata(&old_temp)
                .unwrap()
                .file_type()
                .is_symlink());
            assert!(path.is_file());

            std::fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&unrelated, &path).unwrap();
            assert!(flush().is_err());
            assert!(std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(
                std::fs::read_to_string(&unrelated).unwrap(),
                "keep this file\n"
            );
        }
        std::fs::remove_dir_all(config_dir().unwrap()).unwrap();
    }

    #[test]
    fn usefulness_counts_accept_only_known_aggregate_fields() {
        assert_eq!(parse_usefulness("completion_sessions\t8\ncompletion_accepted\t3\nprivateword\t999\ncompletion_cycle_taps\tinvalid\n"), [8, 0, 3, 0, 0, 0, 0]);
    }

    #[test]
    fn rule_statistics_keep_only_fixed_tags_and_counts() {
        let mut counts = [[0; 2]; RULES.len()];
        counts[0] = [7, 2];
        counts[3] = [4, 1];
        let text = rule_stats_text(counts);
        assert_eq!(parse_rule_stats(&text), counts);
        assert!(!text.contains("privateword"));
        assert_eq!(
            parse_rule_stats("privateword\t10\t1\n"),
            [[0; 2]; RULES.len()]
        );
    }

    #[test]
    fn dirty_flush_skips_clean_data_and_preserves_retries_and_new_changes() {
        let dirty = AtomicBool::new(false);
        flush_if_dirty(&dirty, || panic!("clean data must not be written"));
        dirty.store(true, Ordering::Relaxed);
        flush_if_dirty(&dirty, || Err(std::io::Error::other("save failed")));
        assert!(dirty.load(Ordering::Relaxed));
        flush_if_dirty(&dirty, || {
            dirty.store(true, Ordering::Relaxed);
            Ok(())
        });
        assert!(
            dirty.load(Ordering::Relaxed),
            "new changes must survive a save"
        );
        flush_if_dirty(&dirty, || Ok(()));
        assert!(!dirty.load(Ordering::Relaxed));
    }

    #[test]
    fn frequent_words_receive_a_larger_bounded_boost() {
        assert_eq!(boost_for_count(0), 1.0);
        assert!(boost_for_count(2) > boost_for_count(1));
        assert_eq!(boost_for_count(10), 2.0);
        assert_eq!(boost_for_count(10_000), 2.0);
    }

    #[test]
    fn personal_maps_stop_at_their_documented_limit() {
        let freq: String = (0..=MAX_PERSONAL_ENTRIES)
            .map(|i| format!("word{i}\t1\n"))
            .collect();
        assert_eq!(parse_freq_file(&freq).len(), MAX_PERSONAL_ENTRIES);

        let confusions: String = (0..=MAX_PERSONAL_ENTRIES)
            .map(|i| format!("typed{i}\tcorrected{i}\t1\n"))
            .collect();
        let parsed = parse_confusions_file(&confusions);
        assert_eq!(
            parsed.values().map(HashMap::len).sum::<usize>(),
            MAX_PERSONAL_ENTRIES
        );

        let mut count = u64::MAX;
        increment(&mut count);
        assert_eq!(count, u64::MAX, "hand-edited counters must not wrap");
    }

    #[test]
    fn learned_replacements_require_a_clear_lead_over_all_alternatives() {
        let now = 100 * CONFUSION_HALF_LIFE;
        let preferred = |votes: &[(&str, u64)]| {
            let inner: HashMap<_, _> = votes
                .iter()
                .map(|(word, count)| {
                    (
                        (*word).to_owned(),
                        Confusion {
                            count: *count,
                            decayed_at: now,
                        },
                    )
                })
                .collect();
            preferred_confusion(&inner, now).cloned()
        };
        assert_eq!(preferred(&[("cake", 1)]), None);
        assert_eq!(preferred(&[("cake", 2)]), Some("cake".into()));
        for votes in [
            vec![("cake", 2), ("cafe", 2)],
            vec![("cake", 2), ("cafe", 1)],
            vec![("cake", 5), ("cafe", 2), ("case", 1)],
            vec![("cake", u64::MAX), ("cafe", u64::MAX)],
        ] {
            assert_eq!(preferred(&votes), None);
        }
        assert_eq!(preferred(&[("cake", 3), ("cafe", 1)]), Some("cake".into()));
        assert_eq!(preferred(&[("cake", 4), ("cafe", 2)]), Some("cake".into()));
    }

    #[test]
    fn explicit_feedback_outweighs_retention_and_undo_removes_three_votes() {
        const CHILD: &str = "RECAST_WEIGHTED_FEEDBACK_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "personal::tests::explicit_feedback_outweighs_retention_and_undo_removes_three_votes",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        record_explicit_confusion("typo", "chosen");
        assert!(confusions_map().lock().unwrap().is_empty());
        Config::update_live(|config| config.personal_enabled = true);

        record_confusion("typo", "passive");
        assert_eq!(personal_correction("typo"), None);
        record_explicit_confusion("typo", "chosen");
        assert_eq!(personal_correction("typo").as_deref(), Some("chosen"));
        {
            let map = confusions_map().lock().unwrap();
            assert_eq!(map["typo"]["passive"].count, 1);
            assert_eq!(map["typo"]["chosen"].count, 3);
        }

        record_explicit_confusion("typo", "chosen");
        reject_confusion("typo", "chosen");
        assert_eq!(confusions_map().lock().unwrap()["typo"]["chosen"].count, 3);
        reject_confusion("typo", "chosen");
        assert_eq!(personal_correction("typo"), None);
        assert_eq!(confusions_map().lock().unwrap()["typo"]["chosen"].count, 0);
        reject_confusion("typo", "passive");
        assert_eq!(confusions_map().lock().unwrap()["typo"]["passive"].count, 0);

        confusions_map()
            .lock()
            .unwrap()
            .get_mut("typo")
            .unwrap()
            .get_mut("chosen")
            .unwrap()
            .count = u64::MAX;
        record_explicit_confusion("typo", "chosen");
        assert_eq!(
            confusions_map().lock().unwrap()["typo"]["chosen"].count,
            u64::MAX
        );
    }

    #[test]
    fn confusion_age_survives_restarts_and_new_votes() {
        let now = 100 * CONFUSION_HALF_LIFE;
        let mut evidence = Confusion {
            count: 8,
            decayed_at: now,
        };
        assert_eq!(evidence.count_at(now - 1), 8);
        assert_eq!(evidence.count_at(now + CONFUSION_HALF_LIFE - 1), 8);
        evidence.decay(now + CONFUSION_HALF_LIFE + 10);
        assert_eq!(evidence.count, 4);
        increment(&mut evidence.count);
        assert_eq!(evidence.count_at(now + 2 * CONFUSION_HALF_LIFE), 2);
        assert_eq!(evidence.count_at(u64::MAX), 0);

        let text = format!("typo\tcake\t8\t{now}\nlegacy\tcafe\t2\ninvalid\tcafe\t2\tbad\n");
        let parsed = parse_confusions_at(&text, now + CONFUSION_HALF_LIFE);
        assert_eq!(
            parsed["legacy"]["cafe"].decayed_at,
            now + CONFUSION_HALF_LIFE
        );
        assert!(!parsed.contains_key("invalid"));
        let saved = confusions_text(&parsed);
        let reloaded = parse_confusions_at(&saved, now + 3 * CONFUSION_HALF_LIFE);
        assert_eq!(parsed, reloaded);
        assert_eq!(
            preferred_confusion(&reloaded["typo"], now + 2 * CONFUSION_HALF_LIFE)
                .map(String::as_str),
            Some("cake")
        );
        assert_eq!(
            preferred_confusion(&reloaded["typo"], now + 3 * CONFUSION_HALF_LIFE),
            None
        );
        let future = parse_confusions_at("typo\tcake\t2\t18446744073709551615", now);
        assert_eq!(future["typo"]["cake"].decayed_at, now);
    }

    #[test]
    fn personal_data_is_off_in_the_shipped_test_config() {
        assert!(!enabled());
        assert_eq!(personal_boost("privateword"), 1.0);
        assert_eq!(personal_correction("privateword"), None);
    }

    #[test]
    fn clearing_removes_only_files_owned_by_recast() {
        let unique = format!(
            "recast-personal-clear-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        create_private_dir(&dir).expect("create private test directory");
        for name in [
            PERSONAL_FREQ_FILE,
            CONFUSIONS_FILE,
            PROFILE_FILE,
            RULE_STATS_FILE,
            USEFULNESS_FILE,
        ] {
            let path = dir.join(name);
            write_private(&path, "sensitive\n").expect("write owned file");
            write_private(&path.with_extension("txt.tmp"), "sensitive snapshot\n")
                .expect("write interrupted save");
            for pid in [std::process::id(), u32::MAX] {
                write_private(
                    &path.with_extension(format!("{pid}.tmp")),
                    "sensitive snapshot\n",
                )
                .expect("write interrupted atomic save");
            }
        }
        let keep = [
            "keep.txt",
            "keep.txt.tmp",
            "freq.txt.tmp.backup",
            "freq.user.tmp",
            "freq.123.tmp.backup",
            "unknown.123.tmp",
            "freq.0.tmp",
        ];
        for name in keep {
            std::fs::write(dir.join(name), "user-owned\n").expect("write unexpected file");
        }

        clear_dir(&dir).expect("clear personal data");
        for name in keep {
            assert_eq!(
                std::fs::read_to_string(dir.join(name)).expect("preserve unexpected file"),
                "user-owned\n"
            );
        }
        for name in [
            PERSONAL_FREQ_FILE,
            CONFUSIONS_FILE,
            PROFILE_FILE,
            RULE_STATS_FILE,
            USEFULNESS_FILE,
        ] {
            assert!(!dir.join(name).exists(), "{name} was not removed");
            assert!(
                !dir.join(name).with_extension("txt.tmp").exists(),
                "{name} snapshot was not removed"
            );
            for pid in [std::process::id(), u32::MAX] {
                assert!(
                    !dir.join(name).with_extension(format!("{pid}.tmp")).exists(),
                    "{name} atomic snapshot was not removed"
                );
            }
        }

        for name in keep {
            std::fs::remove_file(dir.join(name)).expect("remove test file");
        }
        clear_dir(&dir).expect("remove empty personal data directory");
        assert!(!dir.exists());
        clear_dir(&dir).expect("clearing missing data remains harmless");
    }

    #[cfg(unix)]
    #[test]
    fn personal_files_are_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "recast-private-file-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        create_private_dir(&dir).expect("create private test directory");
        let path = dir.join("data.txt");
        std::fs::write(&path, "old data\n").expect("write old personal file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("make old file non-private");
        write_private(&path, "private\n").expect("write private file");
        let mode = std::fs::metadata(&path)
            .expect("private file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_file(path).expect("remove private test file");
        std::fs::remove_dir(dir).expect("remove private test directory");
    }
}
