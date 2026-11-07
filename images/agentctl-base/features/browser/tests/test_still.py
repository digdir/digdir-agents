import unittest

import _path  # noqa: F401
from agent_media import still
from agent_media.still import Still


def samples(seconds, picture):
    """What the comparison measures in a clip SECONDS long that shows PICTURE(index) in the frame of that index: whether
    it differs from LAG and from twice LAG seconds earlier, and from the frame before, where before the clip starts is
    black."""
    def change(index, earlier):
        return 255 if earlier < 0 or picture(index) != picture(earlier) else 2

    lag = round(still.LAG * still.RATE)
    return [(index / still.RATE, change(index, index - lag), change(index, index - 2 * lag), change(index, index - 1))
            for index in range(round(seconds * still.RATE))]


def frame(time):
    """The number of the frame at TIME, at the rate the comparison looks at."""
    return round(time * still.RATE)


class StillTimesTest(unittest.TestCase):
    def still_times(self, seconds, picture):
        return still.still_times(samples(seconds, picture))

    def test_a_screen_that_changes_once_is_still_before_and_after(self):
        self.assertEqual(self.still_times(5, lambda index: index >= frame(2)), [Still(0.0, 1.9), Still(2.0, None)])

    def test_a_last_screen_shown_for_a_moment_is_not_readable(self):
        self.assertEqual(self.still_times(3, lambda index: index >= frame(2.9)), [Still(0.0, 2.8)])

    def test_something_moving_is_change(self):
        # A pointer that appears at 2 s and travels until 3 s is a different picture in every frame.
        picture = lambda index: min(index, frame(3)) if index >= frame(2) else 0  # noqa: E731
        self.assertEqual(self.still_times(5, picture), [Still(0.0, 1.9), Still(3.0, None)])

    def test_a_single_changed_frame_leaves_one_still_time(self):
        self.assertEqual(self.still_times(5, lambda index: index == frame(2)), [Still(0.0, None)])

    def test_noise_is_still(self):
        self.assertEqual(still.still_times([(index / 10, *[still.NOISE - 1] * 3) for index in range(20)]),
                         [Still(0.0, None)])

    def test_a_screen_shown_for_the_shortest_readable_time_counts_wherever_it_falls(self):
        for start in (1.0, 1.1, 1.3, 1.7, 2.2):
            picture = lambda index: (index >= frame(start)) + (index >= frame(start + 0.6))  # noqa: E731
            with self.subTest(start=start):
                self.assertEqual([still.label(time) for time in self.still_times(4, picture)], [
                    f'0.0 to {start - 0.1:.1f} s', f'{start:.1f} to {start + 0.5:.1f} s',
                    f'{start + 0.6:.1f} s to the end'])

    def test_a_clip_that_never_holds_still(self):
        self.assertEqual(self.still_times(3, lambda index: index), [])

    def test_a_cursor_blinking_at_a_steady_rate_is_still(self):
        # On and off for half a second each, then for 0.6 s each, as GTK blinks.
        for half in (5, 6):
            with self.subTest(half=half):
                self.assertEqual(self.still_times(5, lambda index: index // half % 2), [Still(0.0, None)])

    def test_a_cursor_that_blinks_on_the_last_frame_is_still_to_the_end(self):
        self.assertEqual(self.still_times(4.6, lambda index: index // 5 % 2), [Still(0.0, None)])

    def test_a_screen_that_changes_once_beside_a_blinking_cursor_is_still_before_and_after(self):
        picture = lambda index: (index // 5 % 2, index >= frame(2))  # noqa: E731
        self.assertEqual(self.still_times(5, picture), [Still(0.0, 1.9), Still(2.0, None)])

    def test_a_screen_that_gives_way_to_the_one_before_is_a_still_time_of_its_own(self):
        self.assertEqual(self.still_times(5, lambda index: frame(2) <= index < frame(2.8)),
                         [Still(0.0, 1.9), Still(2.0, 2.7), Still(2.8, None)])

    def test_something_circling_once_a_second_is_change(self):
        self.assertEqual(self.still_times(5, lambda index: index % frame(1)), [])

    def test_beside_a_blinking_cursor_a_screen_has_to_hold_twice_lag(self):
        # The screen from 2.0 to 2.8 s matches no frame LAG earlier while the cursor blinks, and so is not seen.
        picture = lambda index: (index // 5 % 2, (index >= frame(2)) + (index >= frame(2.8)))  # noqa: E731
        self.assertEqual(self.still_times(5, picture), [Still(0.0, 1.9), Still(2.8, None)])


class DescribeTest(unittest.TestCase):
    def test_lists_each_still_time(self):
        self.assertEqual(still.describe([Still(0.0, 1.9), Still(2.0, None)]),
                         'still long enough to read: 0.0 to 1.9 s, 2.0 s to the end')

    def test_says_when_a_viewer_cannot_read_anything(self):
        self.assertEqual(still.describe([]), 'never still for 0.5 s, so a viewer cannot read it')


class SheetFramesTest(unittest.TestCase):
    def test_a_clip_rarely_still_gets_evenly_spaced_frames_up_to_the_result(self):
        frames, shown = still.sheet_frames([Still(0.0, 1.9), Still(2.0, None)], 5)
        self.assertEqual([label for _, label in frames], ['0.0 s', '1.0 s', '1.9 s', '2.9 s', '3.8 s', '4.8 s'])
        self.assertEqual(frames[-1][0], 5 - still.SETTLED)
        self.assertEqual(shown, 'evenly spaced frames up to the last frame of the last time')

    def test_a_clip_never_still_ends_on_its_last_frame(self):
        frames, shown = still.sheet_frames([], 6.2)
        self.assertEqual([label for _, label in frames], ['0.0 s', '1.2 s', '2.4 s', '3.6 s', '4.8 s', '6.0 s'])
        self.assertEqual(frames[-1][0], 6.2 - still.SETTLED)
        self.assertEqual(shown, 'evenly spaced frames up to the last frame')

    def test_shows_the_end_of_each_still_time(self):
        frames, shown = still.sheet_frames([Still(0.0, 0.9), Still(1.0, 1.9), Still(2.0, None)], 3)
        self.assertEqual(frames, [(0.7, '0.0 to 0.9 s'), (1.7, '1.0 to 1.9 s'), (2.8, '2.0 s to the end')])
        self.assertEqual(shown, 'the last frame of each of those 3 times')

    def test_shows_the_last_still_time_and_the_five_longest_others_in_order(self):
        # Lengths 1, 3, 2, 3, 1, 2, 2, then the last. Of equal ones, the earlier is shown.
        stills, start = [], 0
        for length in (1, 3, 2, 3, 1, 2, 2):
            stills.append(Still(start, start + length))
            start += length + 1
        stills.append(Still(start, None))
        frames, shown = still.sheet_frames(stills, start + 0.5)
        self.assertEqual([label for _, label in frames], [still.label(stills[index]) for index in (1, 2, 3, 5, 6, 7)])
        self.assertEqual(shown, 'the last frame of the last time and the 5 longest others')

    def test_a_still_time_at_the_start_is_shown_from_its_start(self):
        frames, _ = still.sheet_frames([Still(0.0, 0.1), Still(1.0, 1.9), Still(2.0, None)], 3)
        self.assertEqual(frames[0], (0, '0.0 to 0.1 s'))


if __name__ == '__main__':
    unittest.main()
