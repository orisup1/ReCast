import unittest

from build_phrases import phrase_counts, weights


class PhraseTests(unittest.TestCase):
    def test_counts_keep_sentence_and_punctuation_boundaries(self):
        words, pairs = phrase_counts(["Good morning. Good evening!", "תודה רבה, תודה מראש", "good-morning", "good", "morning"])
        self.assertEqual(pairs, {("good", "morning"): 1, ("good", "evening"): 1,
                                 ("תודה", "רבה"): 1, ("תודה", "מראש"): 1})
        self.assertEqual(words["good"], 4)

    def test_weights_require_evidence_and_remain_bounded(self):
        words, pairs = phrase_counts(["good morning"] * 5 + ["thank you"] * 100 + ["rare word"])
        rows = weights(words, pairs, {"good", "morning", "rare", "word"})
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0][:2], ("good", "morning"))
        self.assertEqual(rows[0][2], 4.0)
        self.assertEqual(rows[0][3:], (5, 5, 5))
        self.assertEqual(weights(words, pairs, {"good"}), [])


if __name__ == "__main__":
    unittest.main()
