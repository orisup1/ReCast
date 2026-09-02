//! Passive intelligence: personal frequency, confusion pairs, typing patterns.
//!
//! All three are built by watching what the user actually does — accepted fixes,
//! undo gestures, completions taken, and raw timing — and written to files in
//! the config directory so they survive restarts. This is deliberately opt-in:
//! word-frequency and confusion files can contain sensitive text even though
//! they never leave the machine.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::complete::config_dir;
use crate::config::Config;

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

fn increment(count: &mut u64) {
    *count = count.saturating_add(1);
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
                if !word.is_empty()
                    && (map.len() < MAX_PERSONAL_ENTRIES || map.contains_key(&word))
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
        // ponytail: keep the first 5,000 unique words; evict the least common
        // only if tail learning proves more useful than a hard memory bound.
        if map.len() >= MAX_PERSONAL_ENTRIES && !map.contains_key(&word) {
            return;
        }
        increment(map.entry(word).or_insert(0));
        drop(map);
        maybe_flush_freq();
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

/// Periodically flush personal frequency to disk.
fn maybe_flush_freq() {
    static WORDS_SINCE_FLUSH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = WORDS_SINCE_FLUSH.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if count >= FLUSH_AFTER_WORDS {
        flush_personal_freq();
        WORDS_SINCE_FLUSH.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Write personal frequency to disk atomically.
fn flush_personal_freq() {
    if !enabled() {
        return;
    }
    let Some(path) = personal_path(PERSONAL_FREQ_FILE) else {
        return;
    };
    let map = match personal_freq_map().lock() {
        Ok(m) => m.clone(),
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
    if write_private(&tmp, &out).is_ok() {
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
    let Some(path) = personal_path(CONFUSIONS_FILE) else {
        return HashMap::new();
    };
    std::fs::read_to_string(&path)
        .ok()
        .as_deref()
        .map(parse_confusions_file)
        .unwrap_or_default()
}

/// Parse `typed<TAB>corrected<TAB>count` lines.
fn parse_confusions_file(text: &str) -> HashMap<String, HashMap<String, u64>> {
    let mut outer: HashMap<String, HashMap<String, u64>> = HashMap::new();
    let mut entries = 0;
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
                    let known = outer
                        .get(&typed)
                        .is_some_and(|corrections| corrections.contains_key(&corrected));
                    if known || entries < MAX_PERSONAL_ENTRIES {
                        let corrections = outer.entry(typed).or_default();
                        if corrections.insert(corrected, count).is_none() {
                            entries += 1;
                        }
                    }
                }
            }
        }
    }
    outer
}

/// Record a confusion pair: the user typed `typed` and it was corrected to `corrected`.
pub fn record_confusion(typed: &str, corrected: &str) {
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
        // ponytail: this O(n) count runs only for a new correction pair; keep a
        // separate counter if adding pairs at the 5,000-entry ceiling is hot.
        if !known
            && map.values().map(HashMap::len).sum::<usize>() >= MAX_PERSONAL_ENTRIES
        {
            return;
        }
        increment(
            map.entry(typed)
                .or_default()
                .entry(corrected)
                .or_insert(0),
        );
        drop(map);
        maybe_flush_confusions();
    }
}

/// Look up the most common correction for `typed` from personal confusions.
/// Returns the corrected word if there's a strong enough signal (count >= 2).
pub fn personal_correction(typed: &str) -> Option<String> {
    if !enabled() {
        return None;
    }
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

/// Periodically flush confusions to disk.
fn maybe_flush_confusions() {
    static CONFUSIONS_SINCE_FLUSH: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);
    let count =
        CONFUSIONS_SINCE_FLUSH.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if count >= FLUSH_AFTER_WORDS {
        flush_confusions();
        CONFUSIONS_SINCE_FLUSH.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Write confusions to disk atomically.
fn flush_confusions() {
    if !enabled() {
        return;
    }
    let Some(path) = personal_path(CONFUSIONS_FILE) else {
        return;
    };
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
    if write_private(&tmp, &out).is_ok() {
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
    /// Press time by held key, for dwell calculation on release.
    presses: HashMap<String, Instant>,
    /// Whether profile has unsaved changes.
    dirty: bool,
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
        profile.dirty = true;
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
        profile.dirty = true;
    }
}

/// Flush typing profile to disk (JSON-ish text).
fn flush_profile() {
    if !enabled() {
        return;
    }
    let Some(path) = personal_path(PROFILE_FILE) else {
        return;
    };
    let mut profile = match typing_profile().lock() {
        Ok(p) => p,
        Err(_) => return,
    };
    if !profile.dirty {
        return;
    }
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
    let tmp = path.with_extension("txt.tmp");
    if write_private(&tmp, &out).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
        profile.dirty = false;
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
    if !enabled() {
        return;
    }
    let Some(dir) = data_dir() else {
        return;
    };
    if create_private_dir(&dir).is_err() {
        return;
    }
    spawn_periodic_flusher();
}

fn enabled() -> bool {
    Config::global().personal_enabled
}

/// Directory containing opt-in personal data, if this OS provides one.
pub fn data_dir() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(PERSONAL_DIR))
}

/// Delete only the three files ReCast owns. The directory removal is
/// non-recursive, so an unexpected user file can never be erased with them.
pub fn clear_data() -> Result<Option<PathBuf>, String> {
    let Some(dir) = data_dir() else {
        return Ok(None);
    };
    clear_dir(&dir)?;
    Ok(Some(dir))
}

fn clear_dir(dir: &std::path::Path) -> Result<(), String> {
    for name in [PERSONAL_FREQ_FILE, CONFUSIONS_FILE, PROFILE_FILE] {
        let path = dir.join(name);
        if let Err(error) = std::fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(format!("{}: {error}", path.display()));
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
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(content.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for name in [PERSONAL_FREQ_FILE, CONFUSIONS_FILE, PROFILE_FILE] {
            write_private(&dir.join(name), "sensitive\n").expect("write owned file");
        }
        let keep = dir.join("keep.txt");
        std::fs::write(&keep, "user-owned\n").expect("write unexpected file");

        clear_dir(&dir).expect("clear personal data");
        assert!(keep.exists(), "an unexpected file must be preserved");
        for name in [PERSONAL_FREQ_FILE, CONFUSIONS_FILE, PROFILE_FILE] {
            assert!(!dir.join(name).exists(), "{name} was not removed");
        }

        std::fs::remove_file(keep).expect("remove test file");
        std::fs::remove_dir(dir).expect("remove test directory");
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
