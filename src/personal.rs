//! Passive intelligence: personal frequency, confusion pairs, typing patterns.
//!
//! All three are built by watching what the user actually does — accepted fixes,
//! undo gestures, completions taken, and raw timing — and written to files in
//! the config directory so they survive restarts. No settings, no opt-in; the
//! data is local, small, and only ever improves the user's own experience.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::complete::config_dir;

/// Where personal data lives.
const PERSONAL_DIR: &str = "personal";

/// Personal frequency file: `word<TAB>count` per line, most frequent first.
const PERSONAL_FREQ_FILE: &str = "freq.txt";

/// Confusion pairs file: `typed<TAB>corrected<TAB>count` per line.
const CONFUSIONS_FILE: &str = "confusions.txt";

/// Typing pattern profile: aggregated statistics, JSON-ish text for readability.
const PROFILE_FILE: &str = "profile.txt";

/// Max entries kept in each file. Kept bounded so a long-running daemon
/// doesn't accumulate unbounded memory/disk.
const MAX_PERSONAL_ENTRIES: usize = 5000;

/// How often to flush dirty state to disk (seconds).
const FLUSH_INTERVAL: Duration = Duration::from_secs(30);

/// Minimum word length to enter personal frequency (filters single letters, etc.).
const MIN_WORD_LEN: usize = 3;

/// How many new words before a forced flush.
const FLUSH_AFTER_WORDS: u64 = 20;

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
    let path = personal_path(PERSONAL_FREQ_FILE);
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
                if !word.is_empty() {
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
    if word.chars().count() < MIN_WORD_LEN {
        return;
    }
    let word = word.trim().to_lowercase();
    if word.is_empty() {
        return;
    }
    if let Ok(mut map) = personal_freq_map().lock() {
        *map.entry(word).or_insert(0) += 1;
        maybe_flush_freq(map.len());
    }
}

/// Get personal rank for `word`. Lower = more common personally. Returns
/// `None` if not in personal list.
pub fn personal_rank(word: &str) -> Option<u64> {
    let word = word.trim().to_lowercase();
    personal_freq_map().lock().ok()?.get(&word).copied()
}

/// Boost a candidate's score in completions/spelling based on personal frequency.
/// Returns a multiplier (>= 1.0) applied to the candidate's value.
pub fn personal_boost(word: &str) -> f32 {
    let rank = match personal_rank(word) {
        Some(r) => r,
        None => return 1.0,
    };
    // Heuristic: top 10 personal words get 2x, decaying to 1.0 by rank 1000.
    let boost = 2.0_f32 * (1.0 - (rank as f32).min(1000.0) / 1000.0).max(0.0);
    1.0 + boost
}

/// Periodically flush personal frequency to disk.
fn maybe_flush_freq(len: usize) {
    static WORDS_SINCE_FLUSH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = WORDS_SINCE_FLUSH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if count >= FLUSH_AFTER_WORDS || len.is_multiple_of(MAX_PERSONAL_ENTRIES) {
        flush_personal_freq();
        WORDS_SINCE_FLUSH.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Write personal frequency to disk atomically.
fn flush_personal_freq() {
    let path = personal_path(PERSONAL_FREQ_FILE);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let map = match personal_freq_map().lock() {
        Ok(m) => m,
        Err(_) => return,
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
    let tmp = path.with_extension("txt.tmp");
    if std::fs::write(&tmp, out).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Confusion pairs (typed -> corrected)
// ─────────────────────────────────────────────────────────────────────────────

/// In-memory confusion map: what the user typed -> what it was corrected to.
fn confusions_map() -> &'static Mutex<HashMap<String, HashMap<String, u64>>> {
    static MAP: OnceLock<Mutex<HashMap<String, HashMap<String, u64>>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(load_confusions()))
}

/// Load confusions file.
fn load_confusions() -> HashMap<String, HashMap<String, u64>> {
    let path = personal_path(CONFUSIONS_FILE);
    std::fs::read_to_string(&path)
        .ok()
        .as_deref()
        .map(parse_confusions_file)
        .unwrap_or_default()
}

/// Parse `typed<TAB>corrected<TAB>count` lines.
fn parse_confusions_file(text: &str) -> HashMap<String, HashMap<String, u64>> {
    let mut outer: HashMap<String, HashMap<String, u64>> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() == 3 {
            if let Ok(count) = parts[2].trim().parse::<u64>() {
                let typed = parts[0].trim().to_lowercase();
                let corrected = parts[1].trim().to_lowercase();
                if !typed.is_empty() && !corrected.is_empty() {
                    outer
                        .entry(typed)
                        .or_default()
                        .insert(corrected, count);
                }
            }
        }
    }
    outer
}

/// Record a confusion pair: the user typed `typed` and it was corrected to `corrected`.
pub fn record_confusion(typed: &str, corrected: &str) {
    if typed.is_empty() || corrected.is_empty() || typed == corrected {
        return;
    }
    let typed = typed.trim().to_lowercase();
    let corrected = corrected.trim().to_lowercase();
    if let Ok(mut map) = confusions_map().lock() {
        *map.entry(typed).or_default().entry(corrected).or_insert(0) += 1;
        maybe_flush_confusions(&map);
    }
}

/// Look up the most common correction for `typed` from personal confusions.
/// Returns the corrected word if there's a strong enough signal (count >= 2).
pub fn personal_correction(typed: &str) -> Option<String> {
    let typed = typed.trim().to_lowercase();
    let map = confusions_map().lock().ok()?;
    let inner = map.get(&typed)?;
    let (best, &count) = inner.iter().max_by_key(|(_, &c)| c)?;
    if count >= 2 {
        Some(best.clone())
    } else {
        None
    }
}

/// Get all corrections for `typed` sorted by count (for potential future use).
pub fn all_personal_corrections(typed: &str) -> Vec<(String, u64)> {
    let typed = typed.trim().to_lowercase();
    confusions_map()
        .lock()
        .ok()
        .and_then(|map| map.get(&typed).cloned())
        .map(|inner| {
            let mut v: Vec<_> = inner.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            v
        })
        .unwrap_or_default()
}

/// Periodically flush confusions to disk.
fn maybe_flush_confusions(_map: &HashMap<String, HashMap<String, u64>>) {
    static CONFUSIONS_SINCE_FLUSH: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);
    let count = CONFUSIONS_SINCE_FLUSH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if count >= FLUSH_AFTER_WORDS {
        flush_confusions();
        CONFUSIONS_SINCE_FLUSH.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Write confusions to disk atomically.
fn flush_confusions() {
    let path = personal_path(CONFUSIONS_FILE);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Clone the map while holding the lock, then release it before writing.
    let map: HashMap<String, HashMap<String, u64>> = match confusions_map().lock() {
        Ok(m) => m.clone(),
        Err(_) => return,
    };
    let mut total_entries = 0;
    let mut out = String::from("# Personal confusion pairs — written by ReCast, safe to edit\n");
    for (typed, inner) in map {
        if total_entries >= MAX_PERSONAL_ENTRIES {
            break;
        }
        let mut entries: Vec<_> = inner.iter().collect();
        entries.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for (corrected, &count) in entries {
            if total_entries >= MAX_PERSONAL_ENTRIES {
                break;
            }
            out.push_str(&typed);
            out.push('\t');
            out.push_str(corrected);
            out.push('\t');
            out.push_str(&count.to_string());
            out.push('\n');
            total_entries += 1;
        }
    }
    let tmp = path.with_extension("txt.tmp");
    if std::fs::write(&tmp, out).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Typing pattern profile (dwell times, digraph latencies)
// ─────────────────────────────────────────────────────────────────────────────

/// In-memory typing profile: aggregated dwell times and digraph intervals.
#[derive(Default)]
struct TypingProfile {
    /// Key -> list of dwell times (press to release) in microseconds.
    dwells: HashMap<String, VecDeque<u64>>,
    /// Digraph (prev_key, key) -> list of intervals in microseconds.
    digraphs: HashMap<(String, String), VecDeque<u64>>,
    /// Total keystrokes seen.
    total_keys: u64,
    /// Last key press time for digraph calculation.
    last_press: Option<(String, Instant)>,
    /// Whether profile has unsaved changes.
    dirty: bool,
}

fn typing_profile() -> &'static Mutex<TypingProfile> {
    static PROFILE: OnceLock<Mutex<TypingProfile>> = OnceLock::new();
    PROFILE.get_or_init(|| Mutex::new(TypingProfile::default()))
}

/// Record a key press for digraph timing.
pub fn record_key_press(key_name: &str) {
    let now = Instant::now();
    let key = key_name.to_lowercase();
    if let Ok(mut profile) = typing_profile().lock() {
        if let Some((prev_key, prev_time)) = profile.last_press.take() {
            let interval = now.saturating_duration_since(prev_time).as_micros() as u64;
            if interval < 1_000_000 {
                // Cap at 1 second to filter pauses
                let digraph_key = (prev_key.clone(), key.clone());
                profile.digraphs.entry(digraph_key.clone()).or_default().push_back(interval);
                // Keep only recent N per digraph
                if let Some(dq) = profile.digraphs.get_mut(&digraph_key) {
                    while dq.len() > 100 {
                        dq.pop_front();
                    }
                }
            }
        }
        profile.last_press = Some((key.clone(), now));
        profile.total_keys += 1;
        profile.dirty = true;
    }
}

/// Record a key release for dwell time.
pub fn record_key_release(key_name: &str, press_time: Instant) {
    let dwell = Instant::now().saturating_duration_since(press_time).as_micros() as u64;
    if dwell > 1_000_000 {
        return; // Filter stuck keys
    }
    let key = key_name.to_lowercase();
    if let Ok(mut profile) = typing_profile().lock() {
        profile.dwells.entry(key.clone()).or_default().push_back(dwell);
        if let Some(dq) = profile.dwells.get_mut(&key) {
            while dq.len() > 100 {
                dq.pop_front();
            }
        }
        profile.dirty = true;
    }
}

/// Get median dwell time for a key (microseconds), or None.
pub fn median_dwell(key_name: &str) -> Option<u64> {
    let key = key_name.to_lowercase();
    let profile = typing_profile().lock().ok()?;
    let dwells = profile.dwells.get(&key)?;
    if dwells.is_empty() {
        return None;
    }
    let mut v: Vec<u64> = dwells.iter().copied().collect();
    v.sort_unstable();
    Some(v[v.len() / 2])
}

/// Get median digraph interval for a pair (microseconds), or None.
pub fn median_digraph(prev_key: &str, key: &str) -> Option<u64> {
    let profile = typing_profile().lock().ok()?;
    let dwells = profile.digraphs.get(&(prev_key.to_lowercase(), key.to_lowercase()))?;
    if dwells.is_empty() {
        return None;
    }
    let mut v: Vec<u64> = dwells.iter().copied().collect();
    v.sort_unstable();
    Some(v[v.len() / 2])
}

/// Global median inter-key interval across all digraphs (microseconds).
/// Used to adapt injection timing for slow/fast typists.
pub fn global_median_interval() -> Option<u64> {
    let profile = typing_profile().lock().ok()?;
    let mut all: Vec<u64> = Vec::new();
    for dq in profile.digraphs.values() {
        all.extend(dq.iter().copied());
    }
    if all.is_empty() {
        return None;
    }
    all.sort_unstable();
    Some(all[all.len() / 2])
}

/// Flush typing profile to disk (JSON-ish text).
fn flush_profile() {
    let path = personal_path(PROFILE_FILE);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let profile = match typing_profile().lock() {
        Ok(p) => p,
        Err(_) => return,
    };
    if !profile.dirty {
        return;
    }
    let mut out = String::from("# Typing profile — written by ReCast\n");
    out.push_str(&format!("total_keys: {}\n", profile.total_keys));
    if let Some(global) = global_median_interval() {
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
    let tmp = path.with_extension("txt.tmp");
    if std::fs::write(&tmp, out).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
        if let Ok(mut p) = typing_profile().lock() {
            p.dirty = false;
        }
    }
}

/// Periodic flush of all personal data.
fn spawn_periodic_flusher() {
    std::thread::Builder::new()
        .name("recast-personal-flush".into())
        .spawn(|| loop {
            std::thread::sleep(FLUSH_INTERVAL);
            flush_personal_freq();
            flush_confusions();
            flush_profile();
        })
        .ok();
}

/// Initialize personal data directory and start background flusher.
pub fn init() {
    if let Some(dir) = config_dir().map(|d| d.join(PERSONAL_DIR)) {
        let _ = std::fs::create_dir_all(&dir);
    }
    spawn_periodic_flusher();
}

/// Build the path for a personal file.
fn personal_path(name: &str) -> PathBuf {
    config_dir()
        .map(|d| d.join(PERSONAL_DIR).join(name))
        .unwrap_or_else(|| PathBuf::from(name))
}