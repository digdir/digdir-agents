import os
import tempfile
import unittest

import _path  # noqa: F401
from agent_desktop import tree
from agent_desktop.geometry import Geometry
from agent_desktop.session import DesktopError, Session


class SettingsTest(unittest.TestCase):
    def test_a_bad_tuning_setting_fails_only_the_commands_that_use_it(self):
        session = Session({'DESKTOP_KEEP_CAPTURES': 'abc', 'DESKTOP_SETTLE_ATTEMPTS': '8.0'})
        self.assertEqual(session.settle_interval, 0.1)
        with self.assertRaisesRegex(DesktopError, "DESKTOP_KEEP_CAPTURES must be an integer, got 'abc'"):
            session.keep_captures
        with self.assertRaisesRegex(DesktopError, "DESKTOP_SETTLE_ATTEMPTS must be an integer, got '8.0'"):
            session.settle_attempts

    def test_a_bad_budget_fails_as_a_command_error(self):
        with self.assertRaisesRegex(DesktopError, 'DESKTOP_MAX_WIDTH must be a positive integer'):
            Session({'DESKTOP_MAX_WIDTH': '0'})

    def test_display_and_files(self):
        session = Session({'DISPLAY': ':5', 'DESKTOP_CAPTURES': '/x', 'AGENT_SESSION_ID': 's1'})
        self.assertEqual(session.display, ':5')
        self.assertEqual(session.observed, '/x/.screenshot-geometry.s1')
        self.assertEqual(session.recording_state, '/x/.recording:5')
        self.assertEqual(session.tool_environ['LC_ALL'], 'C.UTF-8')
        self.assertEqual(Session({'DESKTOP_DISPLAY': ':2', 'DISPLAY': ':1'}).display, ':2')


class AddressedTest(unittest.TestCase):
    def setUp(self):
        self.captures = tempfile.TemporaryDirectory()
        self.session = Session({'DESKTOP_CAPTURES': self.captures.name})
        self.display = Geometry(1600, 900, 1445, 812)
        self.session.geometry = lambda: self.display

    def tearDown(self):
        self.captures.cleanup()

    def test_without_an_observation_coordinates_follow_the_display(self):
        self.assertEqual(self.session.addressed(), self.display)

    def test_coordinates_follow_the_last_observation(self):
        self.session.record_observed(Geometry(1600, 900, 1600, 900))
        self.assertEqual(self.session.addressed(), Geometry(1600, 900, 1600, 900))
        self.assertEqual(os.listdir(self.captures.name), ['.screenshot-geometry'])

    def test_refuses_once_the_display_has_changed_size(self):
        self.session.record_observed(Geometry(1280, 720, 1280, 720))
        with self.assertRaisesRegex(DesktopError, 'display changed from 1280x720 to 1600x900'):
            self.session.addressed()
        self.session.forget_observed()
        self.assertEqual(self.session.addressed(), self.display)


class TreeArgumentsTest(unittest.TestCase):
    def test_json_is_accepted_among_the_application_name(self):
        session = Session({})
        self.assertEqual(tree.wanted_application(session, ['Google', '--json', 'Chrome']), 'google chrome')
        self.assertTrue(session.json)
        self.assertEqual(tree.wanted_application(Session({}), []), '')


if __name__ == '__main__':
    unittest.main()
