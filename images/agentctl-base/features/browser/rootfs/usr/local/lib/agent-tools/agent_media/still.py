"""When a clip holds still long enough to read, and which of its frames show what a viewer reads."""

from collections import namedtuple

# Each part of the picture is compared with the same part LAG seconds earlier, so that a pointer travelling or a word
# being typed counts as change however small it is beside the whole screen. A part is a square PART pixels wide, and a
# change below NOISE, out of 255, is noise: a thin text caret blinking stays below it, and a few typed characters do
# not. A frame that matches the one LAG earlier has held since then, apart from anything shown for less than LAG,
# which no one can read.
#
# A wider caret or a terminal's block cursor blinks above NOISE. Blinking about once a second, it is in its other
# state LAG earlier, so each frame is also compared with the one twice LAG earlier. A frame that matches only that one
# has held since then if what came between was a blink: the picture holds from one frame to the next, as it does not
# while a spinner goes round, and the other state recurs, which the frames about LAG earlier or later show by matching
# their own from twice LAG before. A screen shown once, after which the one before it returns, is not a blink, and is
# a still time of its own when it lasts LEAST.
#
# What this misses: while a cursor blinks, a screen has to hold for twice LAG to be seen. A blink that fades over
# several frames, or one far slower or faster than once a second, can break a still time into pieces. The comparison
# is of brightness, so a change of colour alone, such as a red badge turning green, counts as still.
PART = 32
NOISE = 6
LAG = 0.5
# The comparison looks at this many frames a second.
RATE = 10
# A still time has to last this long for a viewer to read part of it. The comparison cannot see one shorter than LAG,
# so it is no shorter, and is compared with SLACK for the rounding of frame times.
LEAST = 0.5
SLACK = 0.05
# A contact sheet's frames, two to a row, large enough to read most text. More would make a sheet that a harness
# shrinks to fit the image size it accepts, until nothing on it is readable.
SHEET_FRAMES = 6
# A frame this long before a still time ends shows everything that settled in it.
SETTLED = 0.2

# A time the picture holds still, in seconds. END is None when it holds until the clip ends.
Still = namedtuple('Still', 'start end')


def still_times(samples):
    """The times a clip holds still long enough to read, from SAMPLES: (time, recent, older, previous) for each
    compared frame, where each is how much it differs from the frame LAG earlier, twice LAG earlier and the frame
    before, as the largest change of any part, out of 255. A frame with none that far back is compared with black."""
    # Frames are counted at RATE, so that the frame LAG away is found exactly.
    lag = round(LAG * RATE)
    older = {round(time * RATE): change for time, _, change, _ in samples}
    previous = {round(time * RATE): change for time, _, _, change in samples}

    def blink(frame, holding):
        # A blink's first frame holds into the next. The clip's last frame has none, and continues what held before it.
        held = previous[frame] < NOISE or previous.get(frame + 1, 0 if holding else 255) < NOISE
        recurs = any(older.get(frame + away + offset, 255) < NOISE for away in (-lag, lag) for offset in (-1, 0, 1))
        return held and recurs

    stills, holding, last = [], False, 0
    for time, recent, older_change, _ in samples:
        frame = last = round(time * RATE)
        # Until a frame has one LAG seconds back to be compared with, it is compared with black.
        if frame < lag:
            continue
        if recent < NOISE:
            since = frame - lag
        elif older_change < NOISE and blink(frame, holding):
            since = frame - 2 * lag
        else:
            if holding:
                holding = False
                stills[-1] = stills[-1]._replace(end=frame - 1)
            continue
        if holding:
            stills[-1] = stills[-1]._replace(start=min(stills[-1].start, since))
        else:
            holding = True
            stills.append(Still(since, None))
        # A still time that has held since before the one before it ended is one with it.
        while len(stills) > 1 and stills[-1].start <= stills[-2].end:
            stills[-2:] = [Still(min(stills[-2].start, stills[-1].start), None)]
    return [Still(still.start / RATE, None if still.end is None else still.end / RATE) for still in stills
            if (last if still.end is None else still.end) - still.start >= (LEAST - SLACK) * RATE]


def label(still):
    return f'{still.start:.1f} s to the end' if still.end is None else f'{still.start:.1f} to {still.end:.1f} s'


def describe(stills):
    """What a viewer gets to read, as `media check` prints it."""
    if not stills:
        return f'never still for {LEAST} s, so a viewer cannot read it'
    return 'still long enough to read: ' + ', '.join(label(still) for still in stills)


def sheet_frames(stills, duration):
    """The frames for a contact sheet of a clip DURATION seconds long, as (time, label), and what they show.

    The sheet ends on the result: the last frame of the last still time, or of the clip when it is never still. Before
    it come the last frames of the five longest other still times, in order. A clip that holds still fewer than three
    times, such as a scrolling terminal, gets evenly spaced frames up to the result instead.
    """
    def end(still):
        return duration if still.end is None else still.end

    if len(stills) < 3:
        result = max((end(stills[-1]) if stills else duration) - SETTLED, 0)
        times = [result * index / (SHEET_FRAMES - 1) for index in range(SHEET_FRAMES - 1)] + [result]
        shown = 'the last frame of the last time' if stills else 'the last frame'
        return [(time, f'{time:.1f} s') for time in times], f'evenly spaced frames up to {shown}'

    others = sorted(range(len(stills) - 1), key=lambda index: stills[index].start - end(stills[index]))
    shown = [stills[index] for index in sorted(others[:SHEET_FRAMES - 1])] + [stills[-1]]
    frames = [(max(end(still) - SETTLED, 0), label(still)) for still in shown]
    if len(shown) == len(stills):
        return frames, f'the last frame of each of those {len(shown)} times'
    return frames, f'the last frame of the last time and the {len(shown) - 1} longest others'
