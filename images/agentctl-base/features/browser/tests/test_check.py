import contextlib
import io
import unittest
from unittest import mock

import _path  # noqa: F401
from agent_media import check, still
from agent_media.video import MediaError, Probe

# Still until 1.9 s, then a change, then still to the end.
SAMPLES = [(index / 10, *[255 if index < 5 or 20 <= index < 25 else 0] * 3) for index in range(40)]


class CheckTest(unittest.TestCase):
    def check(self, probe, samples=SAMPLES):
        out = io.StringIO()
        with mock.patch('agent_media.video.probe', return_value=probe), \
                mock.patch('agent_media.video.differences', return_value=samples), \
                mock.patch('agent_media.video.sheet') as sheet, contextlib.redirect_stdout(out):
            check.check('take.mp4')
        return out.getvalue(), sheet

    def test_a_clip(self):
        out, sheet = self.check(Probe(1280, 720, 4.0, 1000, True))
        self.assertEqual(out, 'take.mp4: 1280x720, 4.0 s, 1000 bytes\n'
                              '  still long enough to read: 0.0 to 1.9 s, 2.0 s to the end\n'
                              '  contact sheet: take.mp4.preview.png '
                              '(evenly spaced frames up to the last frame of the last time)\n')
        self.assertEqual(sheet.call_args[0][2], 'take.mp4.preview.png')

    def test_a_clip_without_its_length_ends_with_its_last_frame(self):
        _, sheet = self.check(Probe(1280, 720, None, 1000, True))
        self.assertEqual(sheet.call_args[0][1][-1][0], 4.0 - still.SETTLED)

    def test_a_picture_has_no_length_or_sheet(self):
        out, sheet = self.check(Probe(64, 48, None, 158, False))
        self.assertEqual(out, 'take.mp4: 64x48, 158 bytes\n')
        sheet.assert_not_called()

    def test_a_clip_without_frames_is_an_error(self):
        with self.assertRaisesRegex(MediaError, 'found no frames'):
            self.check(Probe(1280, 720, 4.0, 1000, True), samples=[])


class FramesTest(unittest.TestCase):
    def test_writes_each_frame_beside_the_clip(self):
        out = io.StringIO()
        with mock.patch('agent_media.video.probe', return_value=Probe(1280, 720, 10.0, 1000, True)), \
                mock.patch('agent_media.video.frame') as frame, contextlib.redirect_stdout(out):
            check.frames('take.mp4', [2.5, 10.0])
        self.assertEqual(out.getvalue(), 'take.mp4.2.5s.png\ntake.mp4.10s.png\n')
        frame.assert_called_with('take.mp4', 10.0, 'take.mp4.10s.png')

    def test_refuses_a_time_past_the_end(self):
        with mock.patch('agent_media.video.probe', return_value=Probe(1280, 720, 10.0, 1000, True)):
            with self.assertRaisesRegex(MediaError, 'is 10.0 s long, so it has no frame at 12 s'):
                check.frames('take.mp4', [12.0])

    def test_refuses_a_picture(self):
        with mock.patch('agent_media.video.probe', return_value=Probe(64, 48, None, 158, False)):
            with self.assertRaisesRegex(MediaError, 'is a single picture'):
                check.frames('take.png', [0.0])


if __name__ == '__main__':
    unittest.main()
