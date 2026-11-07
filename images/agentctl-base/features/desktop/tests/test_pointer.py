import tempfile
import unittest

import _path  # noqa: F401
from agent_desktop import pointer
from agent_desktop.geometry import Geometry
from agent_desktop.session import DesktopError, Session


class FakeSession(Session):
    """A session whose X tools are recorded rather than run, on a 1600x900 display sent at 800x450."""

    def __init__(self, captures, fail_after=None):
        super().__init__({'DESKTOP_CAPTURES': captures})
        # The command after which every checked command fails.
        self.calls, self.fail_after, self.failing = [], fail_after, False

    def run(self, *command, check=True):
        self.calls.append(command)
        if self.failing and check:
            raise DesktopError(f'{command[1]} failed')
        self.failing = self.failing or command[1] == self.fail_after
        return ''

    def addressed(self):
        return Geometry(1600, 900, 800, 450)

    def sync(self):
        pass


class DragTest(unittest.TestCase):
    def setUp(self):
        self.captures = tempfile.TemporaryDirectory()

    def tearDown(self):
        self.captures.cleanup()

    def test_moves_in_steps_between_press_and_release(self):
        session = FakeSession(self.captures.name)
        pointer.drag(session, ['0', '0', '40', '8'])
        self.assertEqual(session.calls, [
            ('xdotool', 'mousemove', '--', '0', '0'), ('xdotool', 'mousedown', '1'),
            ('xdotool', 'mousemove', '--', '20', '4'), ('xdotool', 'mousemove', '--', '40', '8'),
            ('xdotool', 'mousemove', '--', '60', '12'), ('xdotool', 'mousemove', '--', '80', '16'),
            ('xdotool', 'mouseup', '1')])

    def test_passes_negative_coordinates_as_coordinates(self):
        session = FakeSession(self.captures.name)
        pointer.drag(session, ['0', '0', '-40', '0'])
        moves = [call for call in session.calls if call[1] == 'mousemove']
        self.assertTrue(all(call[2] == '--' for call in moves))
        self.assertTrue(moves[-1][3].startswith('-'))

    def test_refuses_bad_coordinates_before_pressing(self):
        session = FakeSession(self.captures.name)
        with self.assertRaisesRegex(DesktopError, "y must be an integer, got 'abc'"):
            pointer.drag(session, ['1', '1', '2', 'abc'])
        self.assertEqual(session.calls, [])

    def test_releases_the_button_when_a_move_fails(self):
        session = FakeSession(self.captures.name, fail_after='mousedown')
        with self.assertRaisesRegex(DesktopError, 'mousemove failed'):
            pointer.drag(session, ['1', '1', '2', '2'])
        self.assertEqual(session.calls[-2][1], 'mousemove')
        self.assertEqual(session.calls[-1], ('xdotool', 'mouseup', '1'))

    def test_releases_the_button_when_a_signal_arrives_as_it_presses(self):
        session = FakeSession(self.captures.name)
        record = session.run

        def run(*command, check=True):
            record(*command, check=check)
            # SIGTERM, which `desktop` turns into SystemExit, arriving as xdotool presses.
            if command[1] == 'mousedown':
                raise SystemExit(143)
            return ''

        session.run = run
        with self.assertRaises(SystemExit):
            pointer.drag(session, ['1', '1', '2', '2'])
        self.assertEqual(session.calls[-1], ('xdotool', 'mouseup', '1'))


if __name__ == '__main__':
    unittest.main()
