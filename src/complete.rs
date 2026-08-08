//! Auto-complete: finishing a word instead of fixing one.
//!
//! The other two pipelines are *corrections* — they wait for a finished word,
//! decide it is wrong, and rewrite it. This one is the opposite: the word is
//! not finished, nothing is wrong with it, and the user has explicitly asked
//! for the rest of it. That difference is why it lives outside `spell.rs` and
//! why it is allowed to be far less conservative: a completion the user did not
//! want cost them one keypress and is undone by another, while a wrong
//! autocorrect happens without being asked for.
//!
//! Two mechanisms, both keyed off the partial word in the buffer:
//!
//! * [`completions`] — press the completion key mid-word and the word is
//!   filled in. It returns a short *ordered list*, not a single answer, because
//!   the trigger key can be tapped again: the second tap swaps in the next
//!   candidate, and the last one hands back exactly what the user typed. That
//!   is what makes a wrong first guess cost a keypress instead of a deletion,
//!   and it is why the completer is allowed to guess at all. The frequency list
//!   is sorted, so "every common word starting with `hel`" is one contiguous
//!   run of it (see `Freq::for_each_with_prefix`) and ranking them is a short
//!   walk, no index and no allocation per rejected candidate.
//! * [`expand`] — abbreviations the user wrote down themselves in
//!   `<config>/recast/abbrev.txt`, expanded when the word is finished — or
//!   offered as the first completion, since a rule the user wrote by hand
//!   beats anything inferred from a corpus.
//!
//! It also owns the session [`suppress`] list: the words a Ctrl-double-tap undo
//! has taken back, which nothing may correct again until restart.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use crate::config::Config;
use crate::dictionary::{Dict, Freq};

/// Longest partial word we will try to complete. Past this the user is typing
/// an identifier or a URL, not reaching for a word.
const MAX_PREFIX_LEN: usize = 20;

/// How many guesses a cycle offers before coming back round to what the user
/// typed. Small on purpose: past three or four taps, deleting the word and
/// typing it out is faster than hunting, and every extra candidate is a rarer
/// word than the one before it.
pub const MAX_CANDIDATES: usize = 4;

/// Fixed-point scale for [`value`], so the ranking can be done in integers.
const VALUE_SCALE: u64 = 1 << 20;

/// How good a completion is: the keystrokes it would save, weighted by how
/// likely it is to be the word meant.
///
/// Ranking candidates by raw frequency — the obvious thing, and what this did
/// at first — quietly optimises the wrong quantity. The user pressed a key to
/// save typing, and a completion that adds one letter to a five-letter prefix
/// has saved them nothing for that press; one that adds six has saved six. So
/// the offer is worth `letters saved × P(this is the word)`, and with rank as
/// a Zipfian stand-in for the probability (`P ∝ 1/rank`) that is this ratio.
///
/// It only reorders candidates that are already close in frequency: a word ten
/// times commoner than its neighbour still wins on frequency alone, which is
/// why `hel` still completes to `help` rather than to a longer, rarer word.
fn value(saved: usize, rank: u32) -> u64 {
    saved as u64 * VALUE_SCALE / (rank as u64 + 1)
}

/// Completions to offer for the partial word `prefix`, best first.
///
/// Empty when there is nothing worth offering. The user's own abbreviation for
/// the prefix, if they defined one, always comes first.
pub fn completions(prefix: &str, en_dict: Dict, en_freq: Freq) -> Vec<String> {
    let cfg = Config::global();
    if !cfg.complete_enabled {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(MAX_CANDIDATES + 1);
    // An abbreviation is a rule the user wrote by hand — it outranks every
    // guess, exactly as it does when the word is finished (see `plan`).
    if let Some(text) = abbreviation(prefix) {
        out.push(text);
    }
    out.extend(completions_with(
        prefix,
        en_dict,
        en_freq,
        cfg.complete_min_len,
        cfg.complete_max_rank,
    ));
    out
}

/// Core of [`completions`] with the tuning knobs passed in, so tests don't
/// depend on the environment. Abbreviations are not consulted here.
///
/// A candidate must be a *longer* word than what was typed (completing a word
/// to itself is a no-op the caller shouldn't have to unwind), a dictionary word
/// — the frequency list is corpus-derived and full of junk tokens — and common
/// enough to be a word someone reaching for that prefix might mean.
pub fn completions_with(
    prefix: &str,
    en_dict: Dict,
    en_freq: Freq,
    min_len: usize,
    max_rank: u32,
) -> Vec<String> {
    if prefix.len() < min_len
        || prefix.len() > MAX_PREFIX_LEN
        || !prefix.bytes().all(|b| b.is_ascii_lowercase())
    {
        return Vec::new();
    }

    // Kept sorted by descending `value`, truncated to length as it goes, so the
    // scan never holds more than a handful of candidates however long the
    // prefix run is.
    let mut best: Vec<(u64, u32, String)> = Vec::with_capacity(MAX_CANDIDATES + 1);
    en_freq.for_each_with_prefix(prefix, |word, rank| {
        if rank > max_rank || word.len() <= prefix.len() {
            return;
        }
        let value = value(word.len() - prefix.len(), rank);
        // Cheap rejection before the dictionary lookup: if the list is already
        // full of better candidates this one can't get in.
        if best.len() == MAX_CANDIDATES && best[MAX_CANDIDATES - 1].0 >= value {
            return;
        }
        // Checked last: it is the only expensive test, and by here only a
        // handful of candidates per prefix still survive.
        if !en_dict.contains(word) {
            return;
        }
        // Ties (same value, different words) go to the commoner word.
        let at = best.partition_point(|(v, r, _)| (*v, std::cmp::Reverse(*r)) > (value, std::cmp::Reverse(rank)));
        best.insert(at, (value, rank, word.to_string()));
        best.truncate(MAX_CANDIDATES);
    });
    best.into_iter().map(|(_, _, word)| word).collect()
}

/// The expansion configured for `word`, if the user defined one.
///
/// Matching is case-insensitive on the key; the expansion is reproduced exactly
/// as written, and the caller re-applies the capitalization the user typed.
pub fn expand(word: &str) -> Option<String> {
    if !Config::global().complete_enabled || word.is_empty() {
        return None;
    }
    abbreviation(word)
}

/// The expansion the user defined for `key`, if any.
fn abbreviation(key: &str) -> Option<String> {
    abbreviations().lock().ok()?.get(key).cloned()
}

/// Whether the user has declared `word` off limits in
/// `<config>/recast/ignore.txt` — one token per line, `#` for comments.
///
/// The wider the speller's edit budget gets, the more jargon it can reach: an
/// eight-letter token two slips from a top-few-thousand word is exactly what it
/// is built to fix, and `hostname` is exactly that shape. Rather than tune the
/// thresholds until nobody's vocabulary is served, this is the escape hatch for
/// the handful of words each user actually types.
pub fn ignored(word: &str) -> bool {
    ignore_list().lock().is_ok_and(|list| list.contains(word))
}

/// Take `word` off both lists, `ignore.txt` included.
///
/// The counterpart of [`suppress`], and the reason the gesture is worth having
/// in this direction too: a list you can only add to is one you eventually stop
/// trusting. Editing the file is a real edit to something the user owns, so it
/// is done conservatively — only lines that *are* this word are dropped,
/// comments and everything else are copied through untouched, and the write
/// goes via a temporary file so an interrupted save can't leave a half-written
/// list behind.
pub fn unlist(word: &str) {
    let word = word.to_lowercase();
    if let Ok(mut set) = suppressed_words().lock() {
        set.remove(&word);
    }
    let was_in_file = ignore_list()
        .lock()
        .map(|mut list| list.remove(&word))
        .unwrap_or(false);
    if was_in_file {
        remove_from_ignore_file(&word);
    }
}

/// Drop every line of `ignore.txt` that is `word`, keeping the rest verbatim.
fn remove_from_ignore_file(word: &str) {
    let Some(path) = user_path("ignore.txt") else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    // Rename over the original rather than truncating it: the user wrote this
    // file, and a failed write should cost them nothing.
    let tmp = path.with_extension("txt.tmp");
    if std::fs::write(&tmp, without_word(&text, word)).is_ok()
        && std::fs::rename(&tmp, &path).is_err()
    {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// `text` without the lines that list `word`. Everything else — comments,
/// blanks, spacing, the other entries — is copied through exactly as written:
/// this is the user's file, and the gesture has a mandate for one line of it.
fn without_word(text: &str, word: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim();
        // A comment is never a listing, whatever it says.
        if !trimmed.starts_with('#') && trimmed.to_lowercase() == word {
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    kept
}

/// Words the user has taken back with the undo gesture this session.
///
/// Undo has to do more than put the letters back. A correction is a *function*
/// of what was typed: retype the same word and the same pipeline reaches the
/// same conclusion, so an undo that only rewrites the screen leaves the user on
/// a treadmill — which is what made the previous escape hatch (edit
/// `ignore.txt`, restart the daemon) the only real one. Undoing a word
/// therefore also retires it: nothing corrects it again until restart.
///
/// Deliberately not persisted: this is the list you land on by reflex, and
/// `ignore.txt` is the one you land on by deciding. [`unlist`] clears entries
/// from both.
fn suppressed_words() -> &'static Mutex<SuppressList> {
    static WORDS: OnceLock<Mutex<SuppressList>> = OnceLock::new();
    WORDS.get_or_init(|| Mutex::new(SuppressList::default()))
}

/// How many undone words are remembered at once.
///
/// The list grew without limit before: every undo added an entry and only the
/// explicit un-ignore gesture ever removed one, so a long-running daemon —
/// which is how this program is meant to run, for weeks — accumulated a word
/// per undo forever. Nothing here is worth unbounded memory.
///
/// 256 is far past what the list is for. It exists so that a word you just
/// took back is not corrected again on the next line; a word you undid two
/// hundred words ago and have not typed since is one `ignore.txt` should be
/// holding instead, which is the gesture's other half.
const MAX_SUPPRESSED: usize = 256;

/// Undone words, newest kept: a set for the lookup, and the order they arrived
/// in so the oldest can be dropped once the list is full.
#[derive(Default)]
struct SuppressList {
    set: HashSet<String>,
    order: std::collections::VecDeque<String>,
}

impl SuppressList {
    fn insert(&mut self, word: String) {
        if !self.set.insert(word.clone()) {
            return; // already listed; leave its position alone
        }
        self.order.push_back(word);
        while self.order.len() > MAX_SUPPRESSED {
            if let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
            }
        }
    }

    fn remove(&mut self, word: &str) {
        if self.set.remove(word) {
            self.order.retain(|w| w != word);
        }
    }

    fn contains(&self, word: &str) -> bool {
        self.set.contains(word)
    }
}

/// Stop correcting `word` for the rest of the session (see
/// [`suppressed_words`]). Called by the undo gesture with the reading the user
/// actually typed.
pub fn suppress(word: &str) {
    if word.is_empty() {
        return;
    }
    if let Ok(mut set) = suppressed_words().lock() {
        set.insert(word.to_lowercase());
    }
}

/// Whether `word` has been undone this session.
pub fn suppressed(word: &str) -> bool {
    suppressed_words()
        .lock()
        .is_ok_and(|set| set.contains(&word.to_lowercase()))
}

/// Path of a user list: `<config dir>/recast/<name>`.
pub fn user_path(name: &str) -> Option<std::path::PathBuf> {
    Some(config_dir()?.join(name))
}

/// Where ReCast keeps the user's files — `~/.config/recast` and its
/// per-OS equivalents.
pub fn config_dir() -> Option<std::path::PathBuf> {
    Some(dirs::config_dir()?.join("recast"))
}

/// The ignore list, read from disk on first use. Behind a `Mutex` rather than
/// straight in a `OnceLock` because it is not immutable for the life of the
/// process: [`unlist`] takes entries out of it, [`ignore_word`] puts them in,
/// and [`reload_user_files`] replaces it wholesale when the file is edited.
fn ignore_list() -> &'static Mutex<HashSet<String>> {
    static LIST: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    LIST.get_or_init(|| Mutex::new(parse_ignore_list(&read_user_file("ignore.txt"))))
}

/// Parse the ignore file: one word per line, `#` starting a comment, folded to
/// lowercase to match the (lowercase) reading of the key buffer.
fn parse_ignore_list(text: &str) -> std::collections::HashSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_lowercase)
        .collect()
}

/// The abbreviation table, read on first use and again whenever the file
/// changes. A missing or unreadable file simply means "no abbreviations" — this
/// is an optional convenience, not something worth failing startup or nagging
/// about.
fn abbreviations() -> &'static Mutex<HashMap<String, String>> {
    static TABLE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(parse_abbreviations(&read_user_file("abbrev.txt"))))
}

/// The text of one of the user's list files, or empty if it isn't there.
fn read_user_file(name: &str) -> String {
    user_path(name)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
}

/// Re-read `abbrev.txt` and `ignore.txt` from disk.
///
/// The session's undo list is deliberately left alone: those are words the user
/// took back with a gesture minutes ago, and a reload is a statement about the
/// files, not about that.
pub fn reload_user_files() {
    if let Ok(mut table) = abbreviations().lock() {
        *table = parse_abbreviations(&read_user_file("abbrev.txt"));
    }
    if let Ok(mut list) = ignore_list().lock() {
        *list = parse_ignore_list(&read_user_file("ignore.txt"));
    }
}

/// Add `word` to the ignore list and to `ignore.txt`, so nothing corrects it
/// again — the counterpart of [`unlist`], for the user who has just seen a
/// correction they never want repeated.
///
/// Appends rather than rewrites: the file belongs to the user, and adding a
/// line is the smallest possible edit to it.
///
/// Only the tray's recent-corrections list calls this, so it is gated to the
/// platforms that have a tray; on Linux the same job is done by the Ctrl
/// double-tap and by editing the file.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn ignore_word(word: &str) {
    let word = word.trim().to_lowercase();
    if word.is_empty() {
        return;
    }
    let already = ignore_list()
        .lock()
        .map(|mut list| !list.insert(word.clone()))
        .unwrap_or(true);
    if already {
        return;
    }
    let Some(path) = user_path("ignore.txt") else {
        return;
    };
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(appended_line(&existing, &word).as_bytes());
    }
}

/// What to append to `existing` to list `word`.
///
/// A file whose last line has no newline of its own would otherwise gain a
/// line reading `previouswordnewword`, listing neither — and the user's last
/// entry would stop working, which is a strange thing to have happen from
/// clicking a menu item about a different word.
///
/// Only [`ignore_word`] calls it, so it is dead on the platforms without a
/// tray — but it is pure, so it is still tested there.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows")),
    allow(dead_code)
)]
fn appended_line(existing: &str, word: &str) -> String {
    let lead = if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" };
    format!("{lead}{word}\n")
}

/// How many abbreviations and ignored words are loaded — what `--status`
/// reports, so a user who has just edited a file can see it took.
pub fn list_counts() -> (usize, usize) {
    (
        abbreviations().lock().map(|t| t.len()).unwrap_or(0),
        ignore_list().lock().map(|l| l.len()).unwrap_or(0),
    )
}

/// How often the user's list files are checked for edits, where they have to be
/// checked at all. Slow enough to be cheap (two `stat`s), fast enough that
/// adding an abbreviation and typing it feels like the same action.
const WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// The files this watches, and the only ones the user is meant to edit.
const WATCHED: [&str; 2] = ["abbrev.txt", "ignore.txt"];

/// Watch `abbrev.txt` and `ignore.txt` for edits and reload them in place.
///
/// Reading these once at startup made the files a restart away from taking
/// effect, which for `abbrev.txt` is most of the cost of using it at all: an
/// abbreviation is written *because* you are about to type it.
///
/// On Linux this blocks on `inotify` and costs nothing at all until something
/// changes. Everywhere else it polls — see [`poll_watch`] for why that is worth
/// the difference rather than a filesystem-watch dependency on three platforms.
pub fn spawn_watcher() {
    // Named so that a stray thread in `top -H` or a debugger identifies itself
    // instead of showing up as another anonymous copy of the process name.
    let spawned = std::thread::Builder::new()
        .name("recast-watch".into())
        .spawn(watch_forever);
    // A thread that cannot be created is not worth failing startup over: the
    // lists still load once at startup and the tray's "Reload lists" still
    // works. Only the automatic pickup is lost.
    if spawned.is_err() {
        eprintln!("Could not start the list watcher — edits to abbrev.txt and ignore.txt will need a reload.");
    }
}

/// Block until one of the watched files changes, reload, repeat.
#[cfg(target_os = "linux")]
fn watch_forever() {
    use nix::sys::inotify::{AddWatchFlags, InitFlags, Inotify};

    // The *directory*, not the two files. Editors overwhelmingly save by
    // writing a temporary file and renaming it over the target, which replaces
    // the inode — a watch on the file itself would survive exactly one save and
    // then be watching something that no longer has a name.
    let Some(dir) = config_dir() else {
        return poll_watch();
    };
    // It may not exist yet: the user has never written either file. Creating it
    // is reasonable here — it is our own directory, and the alternative is
    // watching nothing until a restart that happens to come after they save.
    if std::fs::create_dir_all(&dir).is_err() {
        return poll_watch();
    }

    let Ok(inotify) = Inotify::init(InitFlags::empty()) else {
        return poll_watch();
    };
    // CLOSE_WRITE catches an in-place save, MOVED_TO the rename-over kind,
    // CREATE a first-ever write, DELETE a list emptied by removing the file.
    let flags = AddWatchFlags::IN_CLOSE_WRITE
        | AddWatchFlags::IN_MOVED_TO
        | AddWatchFlags::IN_CREATE
        | AddWatchFlags::IN_DELETE;
    if inotify.add_watch(&dir, flags).is_err() {
        return poll_watch();
    }

    loop {
        // Blocks. No timer, no wakeups, nothing scheduled — the whole point of
        // this over the poll it replaces.
        let Ok(events) = inotify.read_events() else {
            // The watch descriptor is gone (the directory was deleted, or the
            // filesystem does not support inotify after all). Polling still
            // works on whatever replaces it.
            return poll_watch();
        };
        let ours = events.iter().any(|e| {
            e.name
                .as_ref()
                .and_then(|n| n.to_str())
                .is_some_and(|n| WATCHED.contains(&n))
        });
        if ours {
            reload_user_files();
        }
    }
}

/// Modification-time polling, for the platforms without a watch this cheap.
///
/// macOS and Windows both have an equivalent — FSEvents and
/// `ReadDirectoryChangesW` — but each is a chunk of FFI, and the thing being
/// saved is two `stat`s every couple of seconds on files that are almost always
/// absent. That is not the same trade as on Linux, where the daemon is expected
/// to run for weeks and this was the only thing keeping it from being fully
/// idle.
#[cfg(not(target_os = "linux"))]
fn watch_forever() {
    poll_watch()
}

fn poll_watch() {
    let stamp = || {
        WATCHED.map(|name| {
            user_path(name)
                .and_then(|p| std::fs::metadata(p).ok())
                .and_then(|m| m.modified().ok())
        })
    };
    let mut last = stamp();
    loop {
        std::thread::sleep(WATCH_INTERVAL);
        let now = stamp();
        if now != last {
            last = now;
            reload_user_files();
        }
    }
}

/// Parse the abbreviation file: one `abbreviation = expansion` per line, `=` or
/// a tab as the separator, `#` starting a comment line. Keys are lowercased
/// (the buffer only ever holds lowercase readings) and blank or malformed lines
/// are skipped rather than rejected — a typo in the file should cost the user
/// that one line, not the whole table.
fn parse_abbreviations(text: &str) -> HashMap<String, String> {
    let mut table = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=').or_else(|| line.split_once('\t')) else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if key.is_empty() || value.is_empty() {
            continue;
        }
        table.insert(key.to_lowercase(), value.to_string());
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(words: &[&str]) -> Dict {
        Dict::of(words)
    }

    fn freq(entries: &[(&str, u32)]) -> Freq {
        Freq::of(entries)
    }

    /// Every candidate, in offer order, under the shipped defaults
    /// (`Config::from_env`).
    fn offers(prefix: &str, d: Dict, f: Freq) -> Vec<String> {
        completions_with(
            prefix,
            d,
            f,
            crate::config::DEFAULT_COMPLETE_MIN_LEN,
            crate::config::DEFAULT_COMPLETE_MAX_RANK,
        )
    }

    /// What the first tap of the completion key puts on screen.
    fn finish(prefix: &str, d: Dict, f: Freq) -> Option<String> {
        offers(prefix, d, f).into_iter().next()
    }

    #[test]
    fn completes_to_the_most_common_word_with_that_prefix() {
        let d = dict(&["hello", "help", "helmet"]);
        let f = freq(&[("hello", 500), ("help", 140), ("helmet", 9_000)]);
        assert_eq!(finish("hel", d, f).as_deref(), Some("help"));
    }

    #[test]
    fn a_completion_must_be_a_dictionary_word() {
        // The frequency list is corpus-derived and full of junk tokens; a
        // completion has to be a word, not merely something people have typed.
        let d = dict(&["helmet"]);
        let f = freq(&[("helo", 100), ("helmet", 9_000)]);
        assert_eq!(finish("hel", d, f).as_deref(), Some("helmet"));
    }

    #[test]
    fn a_rare_completion_is_not_offered() {
        let d = dict(&["helot"]);
        let f = freq(&[("helot", 45_000)]);
        assert_eq!(finish("hel", d, f), None);
    }

    #[test]
    fn never_completes_a_word_to_itself() {
        let d = dict(&["help"]);
        let f = freq(&[("help", 140)]);
        assert_eq!(finish("help", d, f), None);
    }

    #[test]
    fn a_prefix_with_no_common_word_is_left_alone() {
        let d = dict(&["hello"]);
        let f = freq(&[("hello", 500)]);
        assert_eq!(finish("zqx", d, f), None);
    }

    #[test]
    fn short_and_non_alphabetic_prefixes_are_skipped() {
        let d = dict(&["hello", "the"]);
        let f = freq(&[("hello", 500), ("the", 0)]);
        // One letter matches thousands of words; completing it is a coin flip.
        assert_eq!(finish("t", d, f), None);
        // Digits mean an identifier, not a word being reached for.
        assert_eq!(finish("hel2", d, f), None);
    }

    #[test]
    fn candidates_come_back_in_offer_order_for_the_cycle() {
        let d = dict(&["help", "hello", "helmet", "helicopter", "helpless"]);
        let f = freq(&[
            ("help", 140),
            ("hello", 500),
            ("helmet", 9_000),
            ("helicopter", 12_000),
            ("helpless", 25_000),
        ]);
        let offers = offers("hel", d, f);
        assert_eq!(offers.first().map(String::as_str), Some("help"));
        assert!(offers.len() <= MAX_CANDIDATES);
        // A tap must never offer the same word twice, or the cycle stalls.
        let unique: std::collections::HashSet<&String> = offers.iter().collect();
        assert_eq!(unique.len(), offers.len());
    }

    #[test]
    fn a_longer_completion_beats_an_equally_common_short_one() {
        // Same frequency, so the tie-break is what the completion is *for*:
        // `tomorrow` saves four keystrokes for the tap, `tomb` saves none worth
        // having. Ranking by frequency alone could not tell these apart.
        let d = dict(&["tomorrow", "tome"]);
        let f = freq(&[("tomorrow", 900), ("tome", 900)]);
        assert_eq!(finish("tom", d, f).as_deref(), Some("tomorrow"));
    }

    #[test]
    fn frequency_still_dominates_a_lopsided_pair() {
        // Four extra letters do not buy a word that nobody types: `tomorrow` is
        // an order of magnitude commoner, so it wins despite saving less.
        let d = dict(&["tomorrow", "tomographies"]);
        let f = freq(&[("tomorrow", 900), ("tomographies", 29_000)]);
        assert_eq!(finish("tomo", d, f).as_deref(), Some("tomorrow"));
    }

    #[test]
    fn undone_words_are_left_alone_for_the_session() {
        assert!(!suppressed("hostname"));
        suppress("Hostname");
        // Folded, because the buffer's reading is always lowercase.
        assert!(suppressed("hostname"));
        assert!(!suppressed("hostnames"));
    }

    #[test]
    fn unlisting_puts_a_word_back_in_play() {
        // The other half of the toggle: what one double-tap retired, the next
        // one on the same word un-retires.
        suppress("postgres");
        assert!(suppressed("postgres"));
        unlist("Postgres");
        assert!(!suppressed("postgres"));
    }

    #[test]
    fn rewriting_the_ignore_file_touches_only_the_listed_word() {
        let before = "# my words\nhostname\n\n  Postgres  \nkubectl\n";
        let after = without_word(before, "postgres");
        assert_eq!(after, "# my words\nhostname\n\nkubectl\n");

        // Comments are copied through even when they read like the word …
        assert_eq!(without_word("# postgres\nfoo\n", "postgres"), "# postgres\nfoo\n");
        // … and a word that is not there leaves the file byte-identical.
        assert_eq!(without_word(before, "redis"), before);
    }

    #[test]
    fn listing_a_word_never_joins_it_to_the_line_before() {
        assert_eq!(appended_line("", "hostname"), "hostname\n");
        assert_eq!(appended_line("kubectl\n", "hostname"), "hostname\n");
        // A last line with no newline of its own gets one first, or both
        // entries would be lost to `kubectlhostname`.
        assert_eq!(appended_line("kubectl", "hostname"), "\nhostname\n");
    }

    #[test]
    fn parses_the_abbreviation_file() {
        let table = parse_abbreviations(
            "# my shortcuts\n\
             btw = by the way\n\
             \n\
             TY\tthank you\n\
             addr=1 Main Street, Tel Aviv\n\
             broken line with no separator\n\
             empty =\n",
        );
        assert_eq!(table.get("btw").map(String::as_str), Some("by the way"));
        // Keys are folded to lowercase to match the (lowercase) key buffer …
        assert_eq!(table.get("ty").map(String::as_str), Some("thank you"));
        // … while the expansion keeps exactly what was written.
        assert_eq!(
            table.get("addr").map(String::as_str),
            Some("1 Main Street, Tel Aviv")
        );
        assert!(!table.contains_key("empty"), "a valueless line is skipped");
        assert_eq!(table.len(), 3, "comments and junk lines are skipped");
    }

    #[test]
    fn parses_the_ignore_list() {
        let list = parse_ignore_list("# jargon\nhostname\n\n  Postgres  \n");
        assert!(list.contains("hostname"));
        assert!(list.contains("postgres"), "trimmed and lowercased");
        assert_eq!(list.len(), 2);
    }
}


/// Against the real embedded lists, the way `spell::real_data` is: the unit
/// tests above pin the *rules*, these pin what the rules actually do to the
/// data we ship. A threshold change that looks harmless in isolation shows up
/// here.
#[cfg(test)]
mod real_data {
    use super::*;
    use crate::dictionary::{en_dict, en_freq};

    fn offers(prefix: &str) -> Vec<String> {
        completions_with(
            prefix,
            en_dict(),
            en_freq(),
            crate::config::DEFAULT_COMPLETE_MIN_LEN,
            crate::config::DEFAULT_COMPLETE_MAX_RANK,
        )
    }

    #[test]
    fn finishes_everyday_words() {
        assert_eq!(offers("tomo").first().map(String::as_str), Some("tomorrow"));
        assert_eq!(offers("gove").first().map(String::as_str), Some("government"));
        assert_eq!(offers("unde").first().map(String::as_str), Some("understand"));
        assert_eq!(offers("recei").first().map(String::as_str), Some("received"));
    }

    #[test]
    fn a_crowded_prefix_offers_a_cycle_worth_of_guesses() {
        // The point of the cycle: `hel` is genuinely ambiguous, so the first
        // guess being wrong has to be cheap rather than unlikely.
        let offers = offers("hel");
        assert_eq!(offers.len(), MAX_CANDIDATES);
        for word in ["hello", "help"] {
            assert!(offers.iter().any(|w| w == word), "{word} missing: {offers:?}");
        }
    }

    #[test]
    fn every_offer_is_longer_than_what_was_typed() {
        // A candidate that saves nothing is worse than no candidate: it costs
        // the tap and hands back the same word.
        for prefix in ["hel", "com", "dev", "imp", "thr", "abo"] {
            for word in offers(prefix) {
                assert!(word.len() > prefix.len(), "{prefix} -> {word}");
                assert!(word.starts_with(prefix), "{prefix} -> {word}");
            }
        }
    }

    #[test]
    fn gibberish_and_identifiers_are_left_alone() {
        assert!(offers("zqxj").is_empty());
        // Wrong-layout Hebrew never reaches here (the completer is English-only
        // by layout), but a prefix that spells nothing must still decline.
        assert!(offers("qwrt").is_empty());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod watch_tests {
    use nix::sys::inotify::{AddWatchFlags, InitFlags, Inotify};

    /// The flag set in `watch_forever` is the whole design decision there, and
    /// getting it wrong fails silently — the watcher runs, blocks, and simply
    /// never notices a save. The two cases below are the two ways editors
    /// actually write a file, and both have to land.
    #[test]
    fn both_kinds_of_save_are_noticed() {
        let dir = std::env::temp_dir().join(format!("recast-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        let inotify = Inotify::init(InitFlags::empty()).expect("inotify");
        let flags = AddWatchFlags::IN_CLOSE_WRITE
            | AddWatchFlags::IN_MOVED_TO
            | AddWatchFlags::IN_CREATE
            | AddWatchFlags::IN_DELETE;
        inotify.add_watch(&dir, flags).expect("watch");

        let named = |events: Vec<nix::sys::inotify::InotifyEvent>| -> Vec<String> {
            events
                .iter()
                .filter_map(|e| e.name.as_ref()?.to_str().map(str::to_owned))
                .collect()
        };

        // 1. Saved in place — what `echo >>` and most simple editors do.
        std::fs::write(dir.join("abbrev.txt"), "btw = by the way\n").expect("write");
        let seen = named(inotify.read_events().expect("events"));
        assert!(
            seen.iter().any(|n| n == "abbrev.txt"),
            "an in-place save went unnoticed: {seen:?}"
        );

        // 2. Written elsewhere and renamed over the target — what vim, emacs
        //    and every "atomic save" does. This is the case a watch on the
        //    *file* would miss, because the inode it was watching is gone.
        let tmp = dir.join(".abbrev.txt.swp");
        std::fs::write(&tmp, "btw = by the way\nomw = on my way\n").expect("write tmp");
        let _ = inotify.read_events().expect("drain the temp file's own events");
        std::fs::rename(&tmp, dir.join("abbrev.txt")).expect("rename over");
        let seen = named(inotify.read_events().expect("events"));
        assert!(
            seen.iter().any(|n| n == "abbrev.txt"),
            "a rename-over save went unnoticed: {seen:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
