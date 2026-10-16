import contextlib
from collections import Counter
import hashlib
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import build_intent
from build_intent import main, ngram_counts, read_sentences, render_counts, sentence_runs


class IntentCountTests(unittest.TestCase):
    def test_whitespace_joins_words_but_punctuation_and_sentences_do_not(self):
        counts = ngram_counts(["Good\tmorning. Good evening!", "good", "morning"])
        self.assertEqual(counts[("good", "morning")], 1)
        self.assertEqual(counts[("good", "evening")], 1)
        self.assertNotIn(("morning", "good"), counts)
        self.assertEqual(counts[("good",)], 3)
        self.assertEqual(counts[("morning",)], 2)

    def test_joined_identifiers_and_mixed_scripts_do_not_supply_fragments(self):
        sentence = (
            "keep foo123bar _hidden snake_case can't mother-in-law "
            "helloשלום cafeé a\u0301b end"
        )
        self.assertEqual(list(sentence_runs(sentence)), [("keep",), ("end",)])
        self.assertEqual(list(sentence_runs("one two שלום עולם three four")), [
            ("one", "two"), ("שלום", "עולם"), ("three", "four"),
        ])

    def test_single_letter_and_length_limits(self):
        self.assertEqual(list(sentence_runs(f"I a {'b' * 32} {'c' * 33} x")), [
            ("i", "a", "b" * 32), ("x",),
        ])

    def test_language_filter_uses_only_matching_script(self):
        sentences = ["hello world שלום עולם"]
        self.assertEqual(ngram_counts(sentences, "eng"), {
            ("hello",): 1, ("world",): 1, ("hello", "world"): 1,
        })
        self.assertEqual(ngram_counts(sentences, "heb"), {
            ("שלום",): 1, ("עולם",): 1, ("שלום", "עולם"): 1,
        })

    def test_denominators_include_sentence_final_prefixes(self):
        counts = ngram_counts(["a b c d"] * 2 + ["a b c", "a b", "a"])
        output = render_counts({"eng": counts, "heb": Counter()})
        rows = self.rows(output)
        self.assertEqual(rows["a b c d"], (2, 3))
        self.assertEqual(rows["a b c"], (3, 4))
        self.assertEqual(rows["a b"], (4, 5))
        self.assertEqual(rows["a"], (5, 14))

    def test_four_grams_distinguish_same_recent_context(self):
        counts = ngram_counts(["money at the bank"] * 3 + ["guitarist at the band"] * 2)
        self.assertEqual(counts[("money", "at", "the", "bank")], 3)
        self.assertEqual(counts[("guitarist", "at", "the", "band")], 2)
        self.assertNotIn(("money", "at", "the", "band"), counts)
        self.assertEqual(counts[("at", "the")], 5)
        self.assertFalse(any(len(gram) > 4 for gram in counts))

    def test_output_is_sorted_pruned_and_deterministic(self):
        english = ngram_counts(["z a b c"] * 2 + ["rare only once"])
        hebrew = ngram_counts(["שלום עולם"] * 2)
        output = render_counts({"eng": english, "heb": hebrew})
        reverse_counts = Counter(dict(reversed(list(english.items()))))
        self.assertEqual(output, render_counts({"heb": hebrew, "eng": reverse_counts}))
        keys = [line.split("\t")[0] for line in output.splitlines() if not line.startswith("#")]
        self.assertEqual(keys, sorted(keys))
        rows = self.rows(output)
        self.assertNotIn("rare", rows)
        for key, (count, _) in rows.items():
            self.assertGreaterEqual(count, 2)
            words = key.split()
            if len(words) > 1:
                self.assertIn(" ".join(words[:-1]), rows)
        self.assertIn("Maximum order: 4", output)
        self.assertIn("only aggregates are bundled", output)
        for name, checksum in build_intent.SOURCES.values():
            self.assertIn(f"https://downloads.wortschatz-leipzig.de/corpora/{name}.tar.gz", output)
            self.assertIn(checksum, output)

    def test_checksum_failure_precedes_tar_read(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "invalid.tar.gz"
            archive.write_bytes(b"not even a tar archive")
            with patch.object(build_intent.tarfile, "open") as tar_open:
                with self.assertRaisesRegex(ValueError, "checksum does not match pinned"):
                    read_sentences(archive, "eng")
                tar_open.assert_not_called()

    def test_cli_check_accepts_identical_output_and_detects_drift(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            english, english_source = self.archive(directory, "eng", "1\tGood morning.\n2\tGood morning.\n")
            hebrew, hebrew_source = self.archive(directory, "heb", "1\tשלום עולם\n2\tשלום עולם\n")
            output = directory / "counts.tsv"
            args = [str(english), str(hebrew), "--output", str(output)]
            with patch.dict(build_intent.SOURCES, {"eng": english_source, "heb": hebrew_source}):
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(main(args + ["--check"]), 1)
                    self.assertEqual(main(args), 0)
                    self.assertEqual(main(args + ["--check"]), 0)
                    output.write_text("stale counts\n", encoding="utf-8")
                    self.assertEqual(main(args + ["--check"]), 1)
                    self.assertEqual(output.read_text(encoding="utf-8"), "stale counts\n")

    def test_archive_member_must_be_regular_file(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "eng.tar.gz"
            name = "test_eng"
            with tarfile.open(archive, "w:gz") as corpus:
                member = tarfile.TarInfo(f"{name}/{name}-sentences.txt")
                member.type = tarfile.SYMTYPE
                member.linkname = "/etc/passwd"
                corpus.addfile(member)
            checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
            with patch.dict(build_intent.SOURCES, {"eng": (name, checksum)}):
                with self.assertRaisesRegex(ValueError, "regular sentence file"):
                    read_sentences(archive, "eng")

    @staticmethod
    def rows(output):
        return {
            key: (int(count), int(context_count))
            for line in output.splitlines() if not line.startswith("#")
            for key, count, context_count in [line.split("\t")]
        }

    @staticmethod
    def archive(directory, language, text):
        name = f"test_{language}"
        path = directory / f"{name}.tar.gz"
        encoded = text.encode("utf-8")
        with tarfile.open(path, "w:gz") as corpus:
            member = tarfile.TarInfo(f"{name}/{name}-sentences.txt")
            member.size = len(encoded)
            corpus.addfile(member, io.BytesIO(encoded))
        return path, (name, hashlib.sha256(path.read_bytes()).hexdigest())


if __name__ == "__main__":
    unittest.main()
