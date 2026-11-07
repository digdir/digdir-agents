import unittest

import _path  # noqa: F401
from agent_desktop import keyboard

KEYMAP = '''    8
   24    	0x0071 (q)	0x0051 (Q)
   38    	0x0061 (a)	0x0041 (A)
   93
   97
  120
'''


class KeymapTest(unittest.TestCase):
    def test_reads_keysyms_and_unmapped_keycodes(self):
        present, spare = keyboard.keymap(KEYMAP)
        self.assertEqual(present, {0x71, 0x51, 0x61, 0x41})
        self.assertEqual(spare, ['8', '93', '97', '120'])


class PlanTest(unittest.TestCase):
    def test_text_the_layout_has_needs_no_binding(self):
        self.assertEqual(keyboard.plan('aAq', {0x61, 0x41, 0x71}, ['8', '93']), [('aAq', [])])

    def test_binds_each_missing_character_once_to_both_levels(self):
        self.assertEqual(keyboard.plan('ÆaÆ', {0x61}, ['8', '93', '97']), [('ÆaÆ', [('8', 0xc6)])])

    def test_characters_beyond_latin_1_are_offset(self):
        self.assertEqual(keyboard.plan('Ж', set(), ['8', '93']), [('Ж', [('8', 0x1000416)])])

    def test_splits_when_spare_keycodes_run_out(self):
        # xdotool keeps one spare keycode for itself.
        self.assertEqual(keyboard.plan('æøå', set(), ['8', '93', '97']),
                         [('æø', [('8', 0xe6), ('93', 0xf8)]), ('å', [('8', 0xe5)])])

    def test_control_characters_are_left_to_xdotool(self):
        self.assertEqual(keyboard.plan('a\nb', {0x61, 0x62}, ['8', '93']), [('a\nb', [])])

    def test_without_spare_keycodes_types_as_is(self):
        self.assertEqual(keyboard.plan('æ', set(), ['8']), [('æ', [])])


if __name__ == '__main__':
    unittest.main()
