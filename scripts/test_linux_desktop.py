"""Selection safety checks without GI, a desktop, or user text."""
import unittest
from linux_desktop import replace_selection


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


if __name__ == '__main__': unittest.main()
