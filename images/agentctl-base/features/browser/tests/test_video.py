import json
import os
import re
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

import _path  # noqa: F401
from agent_media import video
from agent_media.video import MediaError, Probe

ANALYSIS = '''frame:0    pts:0       pts_time:0
lavfi.signalstats.YMAX=255
frame:1    pts:1       pts_time:0.1
lavfi.signalstats.YMAX=2
'''
OLDER = '''frame:0    pts:0       pts_time:0
lavfi.signalstats.YMAX=40
frame:1    pts:1       pts_time:0.1
lavfi.signalstats.YMAX=9
'''
PREVIOUS = '''frame:1    pts:1       pts_time:0.1
lavfi.signalstats.YMAX=3
'''


def ffprobe(info, returncode=0):
    return mock.patch('subprocess.run', return_value=subprocess.CompletedProcess([], returncode, json.dumps(info), ''))


class ProbeTest(unittest.TestCase):
    def setUp(self):
        self.file = tempfile.NamedTemporaryFile()

    def tearDown(self):
        self.file.close()

    def probe(self, container, returncode=0):
        info = {'streams': [{'width': 1280, 'height': 720}], 'format': container}
        with ffprobe(info, returncode):
            return video.probe(self.file.name)

    def test_a_clip(self):
        self.assertEqual(self.probe({'format_name': 'mov,mp4,m4a,3gp,3g2,mj2', 'duration': '58.52', 'size': '389459'}),
                         Probe(1280, 720, 58.52, 389459, True))

    def test_a_clip_that_does_not_record_its_length(self):
        self.assertEqual(self.probe({'format_name': 'matroska,webm', 'size': '10'}), Probe(1280, 720, None, 10, True))

    def test_a_picture(self):
        self.assertFalse(self.probe({'format_name': 'png_pipe', 'size': '10'}).clip)
        self.assertFalse(self.probe({'format_name': 'image2', 'size': '10'}).clip)

    def test_a_file_ffprobe_cannot_read(self):
        with self.assertRaisesRegex(MediaError, 'is not a picture or video ffmpeg can read') as raised:
            self.probe({}, returncode=1)
        self.assertEqual(raised.exception.status, 65)

    def test_a_file_that_cannot_be_opened(self):
        with self.assertRaisesRegex(MediaError, 'cannot read /nowhere/x.mp4') as raised:
            video.probe('/nowhere/x.mp4')
        self.assertEqual(raised.exception.status, 66)


class DifferencesTest(unittest.TestCase):
    def test_compares_each_frame_with_lag_and_twice_lag_earlier_and_the_frame_before(self):
        def ffmpeg(*args):
            graph = args[args.index('-filter_complex') + 1]
            for written, output in zip(re.findall(r'file=([^\[]+)\[', graph), (ANALYSIS, OLDER, PREVIOUS)):
                with open(written, 'w') as f:
                    f.write(output)
            return ''

        with mock.patch.object(video, 'ffmpeg', side_effect=ffmpeg) as called:
            self.assertEqual(video.differences('x.mp4', 32, 0.5, 10), [(0.0, 255.0, 40.0, 255), (0.1, 2.0, 9.0, 3.0)])
        graph = called.call_args[0][called.call_args[0].index('-filter_complex') + 1]
        for part in ('setpts=PTS+0.5/TB', 'setpts=PTS+1.0/TB', 'setpts=PTS+0.1/TB', 'scale=iw/32:ih/32:flags=area'):
            self.assertIn(part, graph)

    def test_reads_the_change_of_each_frame(self):
        self.assertEqual(video.measured(ANALYSIS), [(0.0, 255.0), (0.1, 2.0)])


class FfmpegTest(unittest.TestCase):
    def test_ends_with_the_command_that_runs_it(self):
        # A stand-in for ffmpeg that says which process it is and runs on, run by a command that is then killed.
        with tempfile.TemporaryDirectory() as scratch:
            stand_in, said = os.path.join(scratch, 'ffmpeg'), os.path.join(scratch, 'pid')
            with open(stand_in, 'w') as f:
                f.write(f'#!/bin/sh\necho $$ >{said}.new\nmv {said}.new {said}\nexec sleep 30\n')
            os.chmod(stand_in, 0o755)
            command = subprocess.Popen(
                [sys.executable, '-c', 'import sys; sys.path.insert(0, sys.argv[1]); from agent_media import video; '
                 'video.ffmpeg()', os.path.dirname(os.path.dirname(video.__file__))],
                env=dict(os.environ, PATH=f'{scratch}:{os.environ["PATH"]}'))
            for _ in range(250):
                if os.path.exists(said):
                    break
                time.sleep(0.02)
            with open(said) as f:
                ffmpeg = int(f.read())
            command.kill()
            command.wait()
            for _ in range(100):
                if not running(ffmpeg):
                    break
                time.sleep(0.02)
            else:
                os.kill(ffmpeg, 9)
                self.fail('ffmpeg ran on after the command that ran it was killed')


def running(pid):
    try:
        with open(f'/proc/{pid}/stat') as f:
            return f.read().rsplit(')', 1)[1].split()[0] != 'Z'
    except FileNotFoundError:
        return False


class ScaledTest(unittest.TestCase):
    def test_keeps_the_shape_in_even_pixels(self):
        self.assertEqual(video.scaled(1600, 900, 1280), (1280, 720))
        self.assertEqual(video.scaled(1366, 768, 1001), (1000, 562))

    def test_never_enlarges(self):
        self.assertEqual(video.scaled(1024, 768, 1280), (1024, 768))
        self.assertEqual(video.scaled(1023, 767, 1280), (1022, 766))


if __name__ == '__main__':
    unittest.main()
