#!/usr/bin/env python3
"""Add pinned ESDB and Wiktionary lexical entries without changing frequency ranks."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import unicodedata

ROOT = Path(__file__).resolve().parents[1]
ESDB = "https://raw.githubusercontent.com/en-wl/wordlist-diff/71d7dd07676edb60ade43552e10b41314b7e9287/"
SOURCES = {
    "en_US-large.txt": (ESDB + "en_US-large.txt", "fa1f9a1382df724be887d3a5d2d743095e6f33fc0949ebb22415c1554bb42fa7"),
    "en_GB-large.txt": (ESDB + "en_GB-large.txt", "cde5031d8719181cb03c045c4af1e7c20052e9afe09118c5a5e616beed95ea61"),
    "he-wiktionary.jsonl": ("https://kaikki.org/dictionary/Hebrew/kaikki.org-dictionary-Hebrew.jsonl", "c1c6abf161b8a7ff96922a95bb4ba098373b1e4217cad29b0ca9f002d9394204"),
}
BAD_TAGS = {
    "archaic", "obsolete", "rare", "Biblical-Hebrew", "nonstandard", "misspelling",
    "error-unknown-tag", "error-unrecognized-form", "romanization", "class",
    "inflection-template", "table-tags", "pausal",
}
HEBREW_POS = {
    "noun", "verb", "adj", "adv", "intj", "num", "pron", "conj", "particle",
    "det", "prep", "postp", "article", "contraction",
}


def english_words(text):
    # Expanded upstream word lists already contain their licensed inflections.
    # Possessives remain productive forms; stripping their quote would create
    # false spellings such as abacuss. Do not lowercase proper names into words.
    return {word for word in text.splitlines() if re.fullmatch(r"[a-z]{2,64}", word)}


def hebrew_word(text, construct=False):
    text = "".join(
        char for char in unicodedata.normalize("NFC", text)
        if not ("\u0590" <= char <= "\u05ff" and unicodedata.category(char) == "Mn")
    ).replace("׳", "'").replace("״", '"')
    if construct:
        text = text.removesuffix("־")
    if (2 <= len(text) <= 64 and re.fullmatch(r'''[א-ת]+(?:['"][א-ת]+)*''', text)
            and not any(char in "ךםןףץ" for char in text[:-1])):
        return text
    return None


def hebrew_words(lines):
    """Map normalized forms to source article titles for per-entry attribution."""
    words = {}
    for line in lines:
        entry = json.loads(line)
        if entry.get("lang_code") != "he" or entry.get("pos") not in HEBREW_POS:
            continue
        if not any(not BAD_TAGS.intersection(sense.get("tags", []))
                   for sense in entry.get("senses", [])):
            continue
        for form in [{"form": entry["word"]}, *entry.get("forms", [])]:
            tags = set(form.get("tags", []))
            if BAD_TAGS.intersection(tags):
                continue
            word = hebrew_word(form["form"], "construct" in tags)
            if word:
                words[word] = min(words.get(word, entry["word"]), entry["word"])
    return words


def verified_source(directory, name, download=False):
    url, checksum = SOURCES[name]
    path = directory / name
    if not path.exists() and download:
        directory.mkdir(parents=True, exist_ok=True)
        temporary = path.with_suffix(path.suffix + ".download")
        subprocess.run(["curl", "--fail", "--location", "--silent", "--show-error",
                        "--max-time", "120", "--output", str(temporary), url], check=True)
        if hashlib.sha256(temporary.read_bytes()).hexdigest() != checksum:
            temporary.unlink()
            raise ValueError(f"{name}: checksum mismatch; review a new source snapshot before importing")
        temporary.replace(path)
    if hashlib.sha256(path.read_bytes()).hexdigest() != checksum:
        raise ValueError(f"{name}: checksum mismatch")
    return path


def append_words(path, words, check=False):
    original = path.read_bytes()
    existing = set(original.decode("utf-8").splitlines())
    additions = sorted(set(words) - existing)
    if additions and not check:
        newline = b"\r\n" if b"\r\n" in original else b"\n"
        separator = b"" if not original or original.endswith(b"\n") else newline
        path.write_bytes(original + separator + newline.join(word.encode() for word in additions) + newline)
    return additions


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache-dir", type=Path, default=ROOT / "target/dictionary-sources")
    parser.add_argument("--download", action="store_true", help="Fetch missing pinned sources using curl")
    parser.add_argument("--check", action="store_true", help="Report missing entries without writing")
    args = parser.parse_args()
    # Verify every source before modifying either dictionary.
    paths = {name: verified_source(args.cache_dir, name, args.download) for name in SOURCES}
    english = set().union(*(english_words(paths[name].read_text(encoding="utf-8"))
                            for name in ("en_US-large.txt", "en_GB-large.txt")))
    with paths["he-wiktionary.jsonl"].open(encoding="utf-8") as source:
        hebrew = hebrew_words(source)
    en_added = append_words(ROOT / "en_dict.txt", english, args.check)
    he_added = append_words(ROOT / "he_dict.txt", hebrew, args.check)
    # Preserve existing attribution on repeated additive imports.
    attribution = ROOT / "licenses/hebrew-wiktionary-entries.tsv"
    rows = set(attribution.read_text(encoding="utf-8").splitlines()) if attribution.exists() else set()
    rows.update(f"{word}\t{hebrew[word]}" for word in he_added)
    if not args.check:
        attribution.write_text("\n".join(sorted(rows)) + "\n", encoding="utf-8")
    print(f"English: {len(en_added)} additions; Hebrew: {len(he_added)} additions")
    if args.check:
        return bool(en_added or he_added)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
