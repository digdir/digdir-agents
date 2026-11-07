"""Tests the `media` command on clips ffmpeg generates, so it needs ffmpeg, which the browser feature installs.

    python3 -I -B end_to_end.py [MEDIA]

MEDIA is the command to test, /usr/local/bin/media by default. Its name keeps it out of `make image-tools-test`, which
runs where ffmpeg may be missing.
"""

import os
import subprocess
import sys
import tempfile
import unittest

MEDIA = os.path.abspath(sys.argv.pop(1)) if len(sys.argv) > 1 else '/usr/local/bin/media'
sys.path.insert(0, os.path.join(os.path.dirname(os.path.realpath(MEDIA)), '..', 'lib', 'agent-tools'))

from agent_media import video  # noqa: E402


def ffmpeg(*args):
    subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', *args], check=True)


def colours(*names):
    """The ffmpeg inputs for a second of each colour in NAMES."""
    return [arg for name in names for arg in ('-f', 'lavfi', '-i', f'color={name}:s=640x360:d=1:r=25')]


class MediaTest(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()

    def tearDown(self):
        self.scratch.cleanup()

    def path(self, name):
        return os.path.join(self.scratch.name, name)

    def clip(self, name, seconds, start, end, moves=False, then=False):
        """Writes NAME.mp4: white, with a black box from START to END seconds that, if MOVES, travels, and if THEN,
        a second box from END on."""
        x = f'100+{300 if moves else 0}*(t-{start})'
        second = f'gte(t,{end})' if then else '0'
        ffmpeg('-f', 'lavfi', '-i', f'color=white:s=640x360:d={seconds}:r=25',
               '-f', 'lavfi', '-i', f'color=black:s=16x24:d={seconds}:r=25',
               '-filter_complex', f"[1]split[box][then];[0][box]overlay=x='{x}':y=150:enable='between(t,{start},{end})'"
               f"[first];[first][then]overlay=x=400:y=150:enable='{second}',format=yuv420p", self.path(f'{name}.mp4'))
        return self.path(f'{name}.mp4')

    def darkest(self, picture, x, y, width, height):
        """The darkest pixel of PICTURE, out of 255, in the WIDTH x HEIGHT region at X Y."""
        result = subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-i', picture, '-vf',
                                 f'crop={width}:{height}:{x}:{y},format=gray', '-f', 'rawvideo', '-'],
                                capture_output=True, check=True)
        return min(result.stdout)

    def media(self, *args):
        return subprocess.run([MEDIA, *args], capture_output=True, text=True)

    def line(self, path, prefix):
        """What `media check` says about PATH after PREFIX."""
        result = self.media('check', path)
        self.assertEqual(result.returncode, 0, result.stderr)
        lines = [line[len(prefix) + 2:] for line in result.stdout.splitlines() if line.startswith(f'  {prefix}')]
        self.assertEqual(len(lines), 1, result.stdout)
        return lines[0]

    def test_a_screen_that_changes_once_is_still_before_and_after(self):
        path = self.clip('once', 5, 2, 5)
        self.assertEqual(self.line(path, 'still long enough to read: '), '0.0 to 1.9 s, 2.0 s to the end')

    def test_a_screen_that_changes_once_near_the_end_ends_its_sheet(self):
        # The box appears after 5/6 of the clip, so a sheet spaced evenly over the whole clip would miss it.
        path = self.clip('late', 6.6, 5.8, 6.6)
        self.assertEqual(self.line(path, 'contact sheet: '),
                         f'{path}.preview.png (evenly spaced frames up to the last frame of the last time)')
        # Two frames to a row, each 800 pixels wide under a strip of 32 for its label, 8 apart. The box shows at
        # 125 to 145 and 187 to 217 of the frame, which is 1.25 times the clip.
        sheet = f'{path}.preview.png'
        self.assertGreater(self.darkest(sheet, 0 + 125, 32 + 187, 20, 30), 200)
        self.assertLess(self.darkest(sheet, 808 + 125, 2 * 490 + 32 + 187, 20, 30), 50)

    def test_a_last_screen_shown_for_a_moment_is_not_readable(self):
        self.assertEqual(self.line(self.clip('brief-end', 3, 2.9, 3), 'still long enough to read: '), '0.0 to 2.8 s')

    def test_something_as_small_as_a_pointer_travelling_is_change(self):
        self.assertEqual(self.line(self.clip('pointer', 5, 2, 3, moves=True), 'still long enough to read: '),
                         '0.0 to 1.9 s, 3.0 s to the end')

    def test_a_screen_shown_for_the_shortest_readable_time_counts_wherever_it_falls(self):
        for start in (1.0, 1.1, 1.3, 1.7, 2.2):
            with self.subTest(start=start):
                path = self.clip(f'brief-{start}', 4, start, round(start + 0.6, 1), then=True)
                self.assertEqual(self.line(path, 'still long enough to read: '),
                                 f'0.0 to {start - 0.1:.1f} s, {start:.1f} to {start + 0.5:.1f} s, '
                                 f'{start + 0.6:.1f} s to the end')

    def overlaid(self, name, seconds, *boxes):
        """Writes NAME.mp4: white, with a black box 16 by 24 at each of BOXES, (x, y, when) where each is an ffmpeg
        expression of the time t."""
        inputs = [arg for _ in boxes for arg in ('-f', 'lavfi', '-i', f'color=black:s=16x24:d={seconds}:r=25')]
        graph, last = [], '0'
        for index, (x, y, when) in enumerate(boxes, 1):
            graph.append(f"[{last}][{index}]overlay=x='{x}':y='{y}':enable='{when}'[v{index}]")
            last = f'v{index}'
        path = self.path(f'{name}.mp4')
        ffmpeg('-f', 'lavfi', '-i', f'color=white:s=640x360:d={seconds}:r=25', *inputs, '-filter_complex',
               ';'.join(graph) + f';[{last}]format=yuv420p', path)
        return path

    def test_a_blinking_block_cursor_is_still(self):
        # On and off for half a second each, as Chromium blinks a caret, and for 0.6 s each, as GTK does.
        for period in (1, 1.2):
            with self.subTest(period=period):
                path = self.overlaid(f'blink-{period}', 5, (100, 150, f'lt(mod(t,{period}),{period / 2})'))
                self.assertEqual(self.line(path, 'still long enough to read: '), '0.0 s to the end')

    def test_a_screen_that_changes_once_beside_a_blinking_cursor_is_still_before_and_after(self):
        path = self.overlaid('blink-change', 5, (100, 150, 'lt(mod(t,1),0.5)'), (400, 40, 'gte(t,2)'))
        self.assertEqual(self.line(path, 'still long enough to read: '), '0.0 to 1.9 s, 2.0 s to the end')

    def test_a_screen_that_gives_way_to_the_one_before_is_a_still_time_of_its_own(self):
        path = self.overlaid('back', 5, (100, 150, 'between(t,1,1.59)'))
        self.assertEqual(self.line(path, 'still long enough to read: '), '0.0 to 0.9 s, 1.0 to 1.5 s, 1.6 s to the end')

    def test_something_circling_once_a_second_is_change(self):
        # As a loading spinner does, which is back where it was a second earlier.
        path = self.overlaid('spinner', 5, ('300+20*cos(2*PI*t)', '150+20*sin(2*PI*t)', '1'))
        self.assertEqual(self.line(path, 'never still for '), '0.5 s, so a viewer cannot read it')

    def test_three_still_times_make_a_sheet_of_them(self):
        path = self.path('three.mp4')
        ffmpeg(*colours('white', 'black', 'gray'), '-filter_complex', '[0][1][2]concat=n=3:v=1,format=yuv420p', path)
        self.assertEqual(self.line(path, 'contact sheet: '),
                         f'{path}.preview.png (the last frame of each of those 3 times)')

    def test_a_sheet_holds_at_most_six(self):
        path = self.path('eight.mp4')
        ffmpeg(*colours('white', 'black', 'gray', 'red', 'white', 'black', 'gray', 'red'),
               '-filter_complex', 'concat=n=8:v=1,format=yuv420p', path)
        self.assertEqual(self.line(path, 'contact sheet: '),
                         f'{path}.preview.png (the last frame of the last time and the 5 longest others)')

    def test_a_picture_has_a_size(self):
        path = self.path('still.png')
        ffmpeg('-f', 'lavfi', '-i', 'color=white:s=64x48', '-frames:v', '1', path)
        result = self.media('check', path)
        self.assertEqual((result.returncode, result.stdout), (0, f'{path}: 64x48, {os.path.getsize(path)} bytes\n'))

    def test_a_file_ffmpeg_cannot_read_is_an_error(self):
        path = self.path('broken.mp4')
        with open(path, 'w') as f:
            f.write('not a video\n')
        result = self.media('check', path)
        self.assertEqual(result.returncode, 65)
        self.assertIn('is not a picture or video ffmpeg can read', result.stderr)

    def test_frames_are_written_at_full_size(self):
        path = self.clip('frames', 3, 1, 3)
        result = self.media('frames', path, '0.5', '2')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [f'{path}.0.5s.png', f'{path}.2s.png'])
        self.assertEqual(video.probe(f'{path}.2s.png')[:2], (640, 360))
        self.assertEqual(self.media('frames', path, '9').returncode, 65)

    def test_encodes_a_clip_browsers_play(self):
        source, out = self.path('source.mkv'), self.path('out.mp4')
        ffmpeg('-f', 'lavfi', '-i', 'color=white:s=641x361:d=1:r=25', '-c:v', 'libx264', '-pix_fmt', 'yuv444p', source)
        video.encode(source, out, 400)
        self.assertEqual(video.probe(out)[:2], (400, 226))


if __name__ == '__main__':
    unittest.main()
