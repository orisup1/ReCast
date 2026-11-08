"""Selection safety checks without GI, a desktop, or user text."""
import unittest
from linux_desktop import replace_selection, replace_suffix


class Target:
    def __init__(self):
        self.value = 'left akuo right'
        self.selected = (5, 4, 'akuo')
        self.fail_insert = False
        self.fail_delete = False
        self.deleted = []

    def selection(self): return self.selected
    def text(self, start, end): return self.value[start:end]
    def insert(self, start, text):
        if self.fail_insert: return False
        self.value = self.value[:start] + text + self.value[start:]
        return True
    def delete(self, start, end):
        self.deleted.append((start, end))
        if self.fail_delete:
            self.fail_delete = False
            return False
        self.value = self.value[:start] + self.value[end:]
        return True
    def select(self, start, end):
        self.selected = (start, end-start, self.value[start:end])
        return True


class SelectionTests(unittest.TestCase):
    def test_round_trip_preserves_surroundings(self):
        target = Target()
        before = target.value
        self.assertTrue(replace_selection(target, target.selection(), 'שלום🙂', lambda: True))
        self.assertEqual(target.value, 'left שלום🙂 right')
        self.assertTrue(replace_selection(target, target.selection(), 'akuo', lambda: True))
        self.assertEqual(target.value, before)

    def test_insert_failure_never_deletes(self):
        target = Target()
        target.fail_insert = True
        self.assertFalse(replace_selection(target, target.selection(), 'שלום', lambda: True))
        self.assertEqual(target.value, 'left akuo right')
        self.assertEqual(target.deleted, [])

    def test_delete_failure_rolls_back_verified_insert(self):
        target = Target()
        target.fail_delete = True
        self.assertFalse(replace_selection(target, target.selection(), 'שלום', lambda: True))
        self.assertEqual(target.value, 'left akuo right')
        self.assertEqual(target.selection(), (5, 4, 'akuo'))

    def test_stale_selection_or_focus_never_edits(self):
        for expected, valid in [((0, 4, 'left'), True), ((5, 4, 'akuo'), False)]:
            target = Target()
            self.assertFalse(replace_selection(target, expected, 'שלום', lambda: valid))
            self.assertEqual(target.value, 'left akuo right')

    def test_focus_change_after_insert_retains_original(self):
        target = Target()
        states = iter([True, False])
        self.assertFalse(replace_selection(target, target.selection(), 'שלום', lambda: next(states)))
        self.assertEqual(target.value, 'left שלוםakuo right')
        self.assertEqual(target.deleted, [])


class SuffixTarget(Target):
    def __init__(self, value='left recieve Next'):
        super().__init__()
        self.value = value
        self.position = len(value)
        self.selected = None

    def caret(self): return self.position
    def has_selection(self): return self.selected is not None
    def set_caret(self, position):
        self.position = position
        return True
    def insert(self, start, text):
        if not super().insert(start, text): return False
        self.position += len(text)
        return True
    def delete(self, start, end):
        if not super().delete(start, end): return False
        self.position -= end - start
        return True


class SuffixTests(unittest.TestCase):
    def test_spelling_and_undo_preserve_suffix_and_surroundings(self):
        for suffix in [' Next', '   next', ' שלום🙂', '']:
            target = SuffixTarget('left recieve' + suffix)
            expected = 'recieve' + suffix
            replacement = 'receive' + suffix
            self.assertTrue(replace_suffix(target, expected, replacement, lambda: True))
            self.assertEqual(target.value, 'left ' + replacement)
            self.assertEqual(target.caret(), len(target.value))
            self.assertTrue(replace_suffix(target, replacement, expected, lambda: True))
            self.assertEqual(target.value, 'left ' + expected)

    def test_middle_of_document_preserves_trailing_text(self):
        target = SuffixTarget('left recieve right')
        target.position = len('left recieve')
        self.assertTrue(replace_suffix(target, 'recieve', 'receive', lambda: True))
        self.assertEqual(target.value, 'left receive right')
        self.assertEqual(target.caret(), len('left receive'))

    def test_mismatch_selection_and_stale_context_never_edit(self):
        for expected, selected, valid in [('wrong', None, True), ('Next', (0, 1, 'l'), True),
                                           ('Next', None, False), ('', None, True)]:
            target = SuffixTarget()
            target.selected = selected
            before = target.value
            self.assertFalse(replace_suffix(target, expected, 'replacement', lambda: valid))
            self.assertEqual(target.value, before)
            self.assertEqual(target.deleted, [])

    def test_failed_insert_and_failed_delete_preserve_original(self):
        for operation in ['fail_insert', 'fail_delete']:
            target = SuffixTarget()
            before = target.value
            setattr(target, operation, True)
            self.assertFalse(replace_suffix(target, 'recieve Next', 'receive Next', lambda: True))
            self.assertEqual(target.value, before)

    def test_pre_edit_refusal_is_distinct_from_an_attempted_insert(self):
        for expected, begins in [('wrong', []), ('recieve Next', [True])]:
            target = SuffixTarget()
            started = []
            target.fail_insert = True
            self.assertFalse(replace_suffix(target, expected, 'receive Next', lambda: True,
                                            lambda: started.append(True)))
            self.assertEqual(started, begins)
            self.assertEqual(target.value, 'left recieve Next')

    def test_input_during_insert_does_not_delete_original_or_new_input(self):
        target = SuffixTarget()
        insert = target.insert
        def type_during_insert(start, text):
            insert(start, text)
            target.value += '!'
            target.position += 1
            return True
        target.insert = type_during_insert
        self.assertFalse(replace_suffix(target, 'recieve Next', 'receive Next', lambda: True))
        self.assertEqual(target.value, 'left recieve Next!')
        self.assertEqual(target.caret(), len(target.value))


if __name__ == '__main__': unittest.main()
