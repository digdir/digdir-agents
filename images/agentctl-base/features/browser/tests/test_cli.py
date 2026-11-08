import contextlib
import io
import unittest
from unittest import mock

import _path  # noqa: F401
from agent_media import cli
from agent_media.video import MediaError


def run(argv):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        status = cli.main(argv)
    return status, out.getvalue(), err.getvalue()


class MainTest(unittest.TestCase):
    def test_without_a_command_prints_usage(self):
        status, _, err = run([])
        self.assertEqual(status, 64)
        self.assertTrue(err.startswith('media <command>'))

    def test_help(self):
        status, out, _ = run(['help'])
        self.assertEqual(status, 0)
        self.assertIn('\n  check FILE...', out)
        self.assertIn('\n  frames FILE TIME...', out)

    def test_an_unknown_command_fails(self):
        status, _, err = run(['preview', 'x.mp4'])
        self.assertEqual(status, 64)
        self.assertTrue(err.startswith("media: unknown command 'preview'\n"))

    def test_check_goes_on_past_a_file_that_fails(self):
        def check(path):
            if path == 'bad.mp4':
                raise MediaError('bad.mp4 is not a picture or video ffmpeg can read')
            print(f'{path}: fine')

        with mock.patch('agent_media.check.check', check):
            status, out, err = run(['check', 'bad.mp4', 'good.mp4'])
        self.assertEqual(status, 65)
        self.assertEqual(out, 'good.mp4: fine\n')
        self.assertEqual(err, 'media: bad.mp4 is not a picture or video ffmpeg can read\n')

    def test_frames_takes_times_in_seconds(self):
        with mock.patch('agent_media.check.frames') as frames:
            self.assertEqual(run(['frames', 'x.mp4', '12.5', '3'])[0], 0)
        frames.assert_called_once_with('x.mp4', [12.5, 3.0])
        for time in ('-1', '1:30', 'end', '1e3'):
            status, _, err = run(['frames', 'x.mp4', time])
            self.assertEqual(status, 64, time)
            self.assertIn(f"got '{time}'", err)

    def test_frames_needs_a_time(self):
        self.assertEqual(run(['frames', 'x.mp4'])[0], 64)


if __name__ == '__main__':
    unittest.main()
