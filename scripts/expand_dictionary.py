#!/usr/bin/env python3
"""Merge frequency-attested SCOWL entries without changing existing words/ranks."""

import argparse
import hashlib
from pathlib import Path
import re

SOURCE = "https://raw.githubusercontent.com/wooorm/dictionaries/8cfea406b505e4d7df52d5a19bce525df98c54ab/dictionaries/en/index.dic"
SHA256 = "f0b1a234bd178bdd01875b2a392a9647f888b8fe879f79c52aae62c2759b3647"
ROOT = Path(__file__).resolve().parents[1]


def additions(source, existing, frequent):
    # Use explicit entries only: Hunspell affix flags are not words. Keep
    # uppercase names and short tokens out of new layout-correction matches.
    words = {line.split("/", 1)[0] for line in source.splitlines()[1:]}
    return sorted(
        word for word in (words & frequent) - existing
        if re.fullmatch(r"[a-z]{4,}", word)
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help=f"Download from {SOURCE}")
    parser.add_argument("--check", action="store_true", help="Fail if additions remain")
    args = parser.parse_args()
    source = args.source.read_bytes()
    if hashlib.sha256(source).hexdigest() != SHA256:
        parser.error("Source checksum does not match the pinned SCOWL dictionary")
    path = ROOT / "en_dict.txt"
    original = path.read_bytes()
    new = additions(
        source.decode("utf-8"),
        set(original.decode("utf-8").splitlines()),
        set((ROOT / "en_freq.txt").read_text(encoding="utf-8").splitlines()),
    )
    if args.check:
        print(f"{len(new)} missing entries")
        return bool(new)
    if new:
        newline = b"\r\n" if b"\r\n" in original else b"\n"
        separator = b"" if not original or original.endswith(b"\n") else newline
        path.write_bytes(original + separator + newline.join(w.encode() for w in new) + newline)
    print(f"Added {len(new)} entries")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
