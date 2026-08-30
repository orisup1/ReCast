// Build script: dictionary preprocessing (every target) + Windows resource
// embedding (Windows target only).
//
// ── Dictionary preprocessing ─────────────────────────────────────────────────
// The word lists ship as plain one-word-per-line text, but the binary embeds a
// *prepared* form of them: ASCII-folded, punctuation-stripped variants added,
// deduplicated and sorted. Doing that here instead of at startup is what lets
// `dictionary.rs` binary-search the embedded blob directly — the program never
// builds a `HashSet`, so ~100 MB of hash-table heap becomes zero, and the
// several hundred milliseconds of startup parsing become nothing at all.
//
// Output formats (all written to `OUT_DIR`, all `\n`-separated, all sorted by
// byte order so a binary search over the lines is valid):
//
//   *_dict.blob   one word per line
//   *_freq.blob   `word\trank` per line, rank = 0-based line index in the
//                 source file (lower = more common)
//
// ── Windows resources ───────────────────────────────────────────────────────
// Turns the Windows binary into a "full app": the executable carries the ReCast
// icon (shown in Explorer, the taskbar, Alt-Tab and the file's Properties) and a
// VERSIONINFO block (product name, version, copyright). This is the Windows
// analogue of the macOS .app bundle's Info.plist + AppIcon.icns — Windows has no
// bundle format, so the identity lives inside the .exe itself.
//
// Only runs when the *target* is Windows, so Linux/macOS builds are unaffected.
// On a native MSVC build winresource uses rc.exe automatically; when
// cross-compiling with the GNU (mingw-w64) toolchain we point it at the
// prefixed windres/ar.

use std::collections::HashMap;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    prepare_dictionaries();
    embed_windows_resources();
}

/// Both word lists and both frequency lists, sorted and folded into the blobs
/// `dictionary.rs` embeds.
fn prepare_dictionaries() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    // `ascii_fold` mirrors what the runtime used to do while parsing: English
    // entries are lowercased, and an apostrophe/quote-stripped variant is added
    // so a typed `dont` matches the entry `don't` (the English keymap can't
    // produce `'`). Hebrew has no case, but its lists do contain `"` in
    // abbreviations, so the stripped variant is added there too.
    for (src, dst) in [
        ("en_dict.txt", "en_dict.blob"),
        ("he_dict.txt", "he_dict.blob"),
    ] {
        println!("cargo:rerun-if-changed={src}");
        let content = read(src);
        let mut words: Vec<String> = Vec::with_capacity(content.len() / 8);
        for line in content.lines() {
            let word = line.trim();
            if word.is_empty() {
                continue;
            }
            let lower = word.to_ascii_lowercase();
            if let Some(stripped) = strip_quotes(&lower) {
                words.push(stripped);
            }
            words.push(lower);
        }
        words.sort_unstable();
        words.dedup();
        write(&out_dir.join(dst), &words.join("\n"));
    }

    // Frequency lists: the rank *is* the line index, so sorting by word means
    // the rank has to be written out alongside it.
    for (src, dst, fold) in [
        ("en_freq.txt", "en_freq.blob", true),
        ("he_freq.txt", "he_freq.blob", false),
    ] {
        println!("cargo:rerun-if-changed={src}");
        let content = read(src);
        // First rank wins for a repeated word (its best/most-common position),
        // matching the old `entry().or_insert(rank)`.
        let mut best: HashMap<String, u32> = HashMap::with_capacity(content.len() / 8);
        let mut rank: u32 = 0;
        for line in content.lines() {
            let word = line.trim();
            if word.is_empty() {
                continue;
            }
            let word = if fold {
                word.to_ascii_lowercase()
            } else {
                word.to_string()
            };
            if let Some(stripped) = strip_quotes(&word) {
                best.entry(stripped).or_insert(rank);
            }
            best.entry(word).or_insert(rank);
            rank += 1;
        }
        let mut entries: Vec<(String, u32)> = best.into_iter().collect();
        entries.sort_unstable();
        let mut blob = String::with_capacity(entries.len() * 12);
        for (word, rank) in &entries {
            blob.push_str(word);
            blob.push('\t');
            blob.push_str(&rank.to_string());
            blob.push('\n');
        }
        blob.pop(); // no trailing newline: every line is a real entry
        write(&out_dir.join(dst), &blob);
    }
}

/// The apostrophe/quote-free variant of `word`, or `None` when there is nothing
/// to strip (or nothing left afterwards).
fn strip_quotes(word: &str) -> Option<String> {
    if !word.bytes().any(|b| b == b'\'' || b == b'"') {
        return None;
    }
    let stripped: String = word.chars().filter(|c| *c != '\'' && *c != '"').collect();
    (!stripped.is_empty()).then_some(stripped)
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {path}: {e}"))
}

fn write(path: &PathBuf, content: &str) {
    std::fs::write(path, content).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
}

fn embed_windows_resources() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    // Rebuild if the icon changes.
    println!("cargo:rerun-if-changed=assets/recast.ico");

    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/recast.ico");
    res.set("ProductName", "ReCast");
    res.set(
        "FileDescription",
        "ReCast — automatic English/Hebrew keyboard-layout correction",
    );
    res.set("OriginalFilename", "ReCast.exe");
    res.set("LegalCopyright", "© 2026 ReCast");
    // FileVersion / ProductVersion default to CARGO_PKG_VERSION, filled in by
    // winresource from the environment.

    // Cross-compiling from a non-Windows host with the GNU toolchain: use the
    // mingw-w64 tools by their target-prefixed names.
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env == "gnu" && !cfg!(target_os = "windows") {
        res.set_windres_path("x86_64-w64-mingw32-windres");
        res.set_ar_path("x86_64-w64-mingw32-ar");
    }

    if let Err(e) = res.compile() {
        // Don't hard-fail the build if the resource compiler is unavailable —
        // the binary still works, it just lacks the embedded icon/metadata.
        println!("cargo:warning=failed to embed Windows resources: {e}");
    }
}
