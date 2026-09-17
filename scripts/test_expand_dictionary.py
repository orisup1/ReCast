"""Run with: python3 -m unittest discover -s scripts"""

import unittest

from expand_dictionary import additions


class ImportTest(unittest.TestCase):
    def test_filters_and_repeated_import(self):
        source = "8\nwebsite/SM\nwebsite/SM\ntexting\nAlice/M\napi\nnoise\nhello\nfoo-bar\n"
        frequent = {"website", "texting", "Alice", "api", "hello", "foo-bar"}
        existing = {"hello"}
        new = additions(source, existing, frequent)
        self.assertEqual(new, ["texting", "website"])
        self.assertEqual(additions(source, existing | set(new), frequent), [])
