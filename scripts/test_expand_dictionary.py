"""Run with: python3 -m unittest discover -s scripts."""

import hashlib
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import json

from expand_dictionary import english_words, hebrew_words, hebrew_word, append_words, verified_source


class ImportTest(unittest.TestCase):
    def test_expanded_english_forms_do_not_require_frequency_evidence(self):
        self.assertEqual(english_words("codebases\nableism\ncolour\nAlice\nword/SM\nword's\nfoo-bar\na\n"),
                         {"codebases", "ableism", "colour"})

    def test_hebrew_normalization_does_not_join_phrases_or_import_metadata(self):
        self.assertEqual(hebrew_word("גִּ׳ירָפָה"), "ג'ירפה")
        self.assertEqual(hebrew_word("בָּתֵּי־", construct=True), "בתי")
        for word in ["בן־אדם", "שתי מילים", "םילה", "עברית1", "aשלום", "־בתי"]:
            self.assertIsNone(hebrew_word(word), word)
        entry = {"word": "דוגמה", "lang_code": "he", "pos": "noun", "senses": [{}],
                 "forms": [{"form": "דוגמאות", "tags": ["plural"]},
                           {"form": "קֶטֶל", "tags": ["class"]},
                           {"form": "בדיוני", "tags": ["error-unrecognized-form"]}]}
        obsolete = {**entry, "word": "מיושן", "senses": [{"tags": ["obsolete"]}]}
        name = {**entry, "word": "ישראל", "pos": "name"}
        self.assertEqual(hebrew_words(map(json.dumps, [entry, obsolete, name])),
                         {"דוגמה": "דוגמה", "דוגמאות": "דוגמה"})

    def test_additive_import_preserves_existing_bytes_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "dictionary.txt"
            original = b"zebra\r\nhello\r\n"
            path.write_bytes(original)
            self.assertEqual(append_words(path, {"hello", "colour"}, check=True), ["colour"])
            self.assertEqual(path.read_bytes(), original)
            self.assertEqual(append_words(path, {"hello", "colour"}), ["colour"])
            self.assertEqual(path.read_bytes(), original + b"colour\r\n")
            self.assertEqual(append_words(path, {"hello", "colour"}), [])

    def test_sources_fail_closed_when_checksum_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "sample.txt"
            path.write_bytes(b"word\n")
            sources = {path.name: ("https://example.invalid/source", hashlib.sha256(path.read_bytes()).hexdigest())}
            with patch("expand_dictionary.SOURCES", sources):
                self.assertEqual(verified_source(Path(directory), path.name), path)
                path.write_bytes(b"different\n")
                with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                    verified_source(Path(directory), path.name)


if __name__ == "__main__":
    unittest.main()
