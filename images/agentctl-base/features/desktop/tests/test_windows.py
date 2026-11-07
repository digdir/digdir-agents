import unittest

import _path  # noqa: F401
from agent_desktop import windows


class ParseTest(unittest.TestCase):
    def test_client_list(self):
        self.assertEqual(windows.client_list('_NET_CLIENT_LIST(WINDOW): window id # 0x200008, 0x600003\n'),
                         ['0x200008', '0x600003'])
        self.assertEqual(windows.client_list('_NET_CLIENT_LIST:  no such atom on any window.\n'), [])

    def test_window_class_is_the_second_string(self):
        self.assertEqual(windows.window_class('WM_CLASS(STRING) = "Navigator", "Chromium-browser"\n'),
                         'Chromium-browser')
        self.assertEqual(windows.window_class('WM_CLASS:  not found.\n'), '')


if __name__ == '__main__':
    unittest.main()
