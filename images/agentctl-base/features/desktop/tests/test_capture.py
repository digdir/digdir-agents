import os
import tempfile
import unittest
from unittest import mock

import _path  # noqa: F401
from agent_desktop import capture
from agent_desktop.session import DesktopError, Session


class OptionsTest(unittest.TestCase):
    def test_out_takes_a_jpeg_path(self):
        self.assertEqual(capture.options('screenshot', []), '')
        self.assertEqual(capture.options('screenshot', ['--out', 'a.jpg']), 'a.jpg')
        with self.assertRaisesRegex(DesktopError, "--out must name a .jpg file, got 'a.png'"):
            capture.options('screenshot', ['--out', 'a.png'])
        with self.assertRaisesRegex(DesktopError, '--out needs a path'):
            capture.options('screenshot', ['--out'])
        with self.assertRaisesRegex(DesktopError, "screenshot: unexpected argument 'x'"):
            capture.options('screenshot', ['x'])


class SettledTest(unittest.TestCase):
    def setUp(self):
        self.captures = tempfile.TemporaryDirectory()
        self.session = Session({'DESKTOP_CAPTURES': self.captures.name, 'DESKTOP_SETTLE_ATTEMPTS': '4'})
        self.out = os.path.join(self.captures.name, 'shot.jpg')

    def tearDown(self):
        self.captures.cleanup()

    def settle(self, frames):
        """Settles over captures that return FRAMES in turn, and returns how many it took and what it kept."""
        frames = iter(frames)

        def grab(session, output, size, offset, scaled=None):
            with open(output, 'wb') as f:
                f.write(next(frames))

        with mock.patch.object(capture, 'grab', grab), mock.patch('time.sleep'):
            taken = capture.settled(self.session, self.out, '10x10', '0,0')
        with open(self.out, 'rb') as f:
            return taken, f.read()

    def test_stops_at_two_identical_frames(self):
        self.assertEqual(self.settle([b'a', b'b', b'b', b'c']), (3, b'b'))
        self.assertEqual(os.listdir(self.captures.name), ['shot.jpg'])

    def test_gives_up_on_a_screen_that_keeps_changing(self):
        self.assertEqual(self.settle([b'a', b'b', b'c', b'd']), (4, b'd'))


class NewPathTest(unittest.TestCase):
    def test_keeps_the_most_recent_captures(self):
        with tempfile.TemporaryDirectory() as captures:
            session = Session({'DESKTOP_CAPTURES': captures, 'DESKTOP_KEEP_CAPTURES': '3'})
            for index in range(5):
                path = os.path.join(captures, f'old-{index}.jpg')
                open(path, 'w').close()
                os.utime(path, (index, index))
            open(os.path.join(captures, 'notes.txt'), 'w').close()
            path = capture.new_path(session, 'screen')
            # Room for the new one: the two newest stay, and what is not a capture is left alone.
            self.assertEqual(sorted(os.listdir(captures)), ['notes.txt', 'old-3.jpg', 'old-4.jpg'])
            self.assertEqual(os.path.dirname(path), captures)
            self.assertRegex(os.path.basename(path), r'^screen-\d{8}-\d{6}-\d{4}\.jpg$')


if __name__ == '__main__':
    unittest.main()
