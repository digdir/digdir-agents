import contextlib
import io
import unittest

import _path  # noqa: F401
from agent_desktop import windows
from agent_desktop.session import Session


class ParseTest(unittest.TestCase):
    def test_client_list(self):
        self.assertEqual(windows.client_list('_NET_CLIENT_LIST(WINDOW): window id # 0x200008, 0x600003\n'),
                         ['0x200008', '0x600003'])
        self.assertEqual(windows.client_list('_NET_CLIENT_LIST:  no such atom on any window.\n'), [])

    def test_window_class_is_the_second_string(self):
        self.assertEqual(windows.window_class('WM_CLASS(STRING) = "Navigator", "Chromium-browser"\n'),
                         'Chromium-browser')
        self.assertEqual(windows.window_class('WM_CLASS:  not found.\n'), '')


class DisplayTest(unittest.TestCase):
    """What `display` prints, as the bash helper printed it on a 1600x900 display with a Norwegian layout."""

    def display(self, environ, json=False):
        session = Session(dict(environ, DISPLAY=':1'), json=json)
        session.run = lambda *command, check=True: (
            'Screen 0: ...\n  dimensions:    1600x900 pixels (423x238 millimeters)\n' if command[0] == 'xdpyinfo'
            else 'rules:      evdev\nmodel:      pc105\nlayout:     no\n')
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            windows.display(session, [])
        return out.getvalue()

    def test_describes_the_budget_and_where_it_comes_from(self):
        self.assertEqual(self.display({'AGENT_HARNESS': 'claudeCode'}), (
            'display :1 is 1600x900; screenshots are sent at 1445x812 (0.90x)\n'
            'screenshot budget 1175000 pixels, a 1568 px longest edge: for harness claudeCode\n'
            'keyboard layout no\n'))
        self.assertIn("the default, for unknown harness 'mystery'", self.display({'AGENT_HARNESS': 'mystery'}))
        self.assertEqual(self.display({'DESKTOP_MAX_WIDTH': '800', 'DESKTOP_MAX_HEIGHT': '600'}).splitlines()[:2], [
            'display :1 is 1600x900; screenshots are sent at 800x450 (0.50x)',
            'screenshot budget a 800x600 box: set by DESKTOP_MAX_WIDTH DESKTOP_MAX_HEIGHT'])

    def test_json_keeps_the_scale_to_two_decimals(self):
        self.assertEqual(self.display({'AGENT_HARNESS': 'codex'}, json=True), (
            '{"display":":1","width":1600,"height":900,"sentWidth":1600,"sentHeight":900,"scale":1.00,"budget":'
            '{"pixels":2457600,"maxEdge":2048,"maxWidth":null,"maxHeight":null,"source":"harness",'
            '"harness":"codex","overrides":[]},"keyboard":{"layout":"no","variant":"","options":""}}\n'))


if __name__ == '__main__':
    unittest.main()
