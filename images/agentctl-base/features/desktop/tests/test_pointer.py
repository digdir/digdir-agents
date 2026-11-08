import tempfile
import unittest
from unittest import mock

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
        # Where the pointer is when the glide to the drop sets off: where the drag pressed.
        self.location = mock.patch.object(pointer, 'location', return_value=(0, 0))
        self.location.start()

    def tearDown(self):
        self.location.stop()
        self.captures.cleanup()

    def test_glides_between_press_and_release(self):
        session = FakeSession(self.captures.name)
        pointer.drag(session, ['0', '0', '40', '8'])
        self.assertEqual(session.calls[:2], [('xdotool', 'mousemove', '--', '0', '0'), ('xdotool', 'mousedown', '1')])
        glide = session.calls[2]
        self.assertGreater(glide.count('mousemove'), 10)
        self.assertEqual(glide[-4:-2], ('80', '16'))
        self.assertEqual(session.calls[3:], [('xdotool', 'mouseup', '1')])

    def test_passes_negative_coordinates_as_coordinates(self):
        session = FakeSession(self.captures.name)
        pointer.drag(session, ['0', '0', '-40', '0'])
        glide = session.calls[2]
        moves = [index for index, word in enumerate(glide) if word == 'mousemove']
        self.assertTrue(all(glide[index + 1] == '--' for index in moves))
        self.assertTrue(glide[-4].startswith('-'))

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


class MoveTest(unittest.TestCase):
    def setUp(self):
        self.captures = tempfile.TemporaryDirectory()

    def tearDown(self):
        self.captures.cleanup()

    def test_jumps_unless_a_recording_is_running(self):
        session = FakeSession(self.captures.name)
        with mock.patch.object(pointer.record, 'running', return_value=False):
            pointer.move(session, ['10', '10'])
        self.assertEqual(session.calls, [('xdotool', 'mousemove', '--', '20', '20')])
        session.calls.clear()
        with mock.patch.object(pointer.record, 'running', return_value=True), \
                mock.patch.object(pointer, 'location', return_value=(0, 0)):
            pointer.move(session, ['10', '10'])
        self.assertGreater(session.calls[0].count('mousemove'), 10)


class GlideTest(unittest.TestCase):
    def test_eases_to_the_target_in_a_fraction_of_a_second(self):
        points, pause = pointer.glide_steps((0, 0), (1500, 0))
        self.assertEqual(points[-1], (1500, 0))
        self.assertAlmostEqual(len(points) * pause, 0.8, places=2)
        # Eased: the first step is shorter than a step in the middle.
        middle = len(points) // 2
        self.assertLess(points[0][0], points[middle][0] - points[middle - 1][0])
        short, short_pause = pointer.glide_steps((0, 0), (10, 0))
        self.assertAlmostEqual(len(short) * short_pause, 0.3, places=2)


if __name__ == '__main__':
    unittest.main()
