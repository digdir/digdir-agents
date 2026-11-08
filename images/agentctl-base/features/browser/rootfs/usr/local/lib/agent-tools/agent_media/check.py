"""Checking a file as someone watching it would see it."""

from . import still, video
from .video import MediaError


def check(path):
    """Prints PATH's size and length. For a clip, also prints when it holds still long enough to read, and writes
    PATH.preview.png, a contact sheet of what a viewer gets to read, which an agent can read as an image."""
    probe = video.probe(path)
    length = f'{probe.duration:.1f} s, ' if probe.clip and probe.duration is not None else ''
    print(f'{path}: {probe.width}x{probe.height}, {length}{probe.size} bytes')
    if not probe.clip:
        return
    samples = video.differences(path, still.PART, still.LAG, still.RATE)
    if not samples:
        raise MediaError(f'ffmpeg found no frames in {path}')
    # A file that does not record its length, such as a WebM written as it was recorded, ends with its last frame.
    duration = probe.duration if probe.duration is not None else samples[-1][0] + 1 / still.RATE
    stills = still.still_times(samples)
    print(f'  {still.describe(stills)}')
    frames, shown = still.sheet_frames(stills, duration)
    sheet = f'{path}.preview.png'
    video.sheet(path, frames, sheet)
    print(f'  contact sheet: {sheet} ({shown})')


def frames(path, times):
    """Writes the frame at each of TIMES, in seconds, to PATH.TIMEs.png at full size, and prints the paths."""
    probe = video.probe(path)
    if not probe.clip:
        raise MediaError(f'{path} is a single picture, not a clip')
    for at in times:
        if probe.duration is not None and at > probe.duration:
            raise MediaError(f'{path} is {probe.duration:.1f} s long, so it has no frame at {at:g} s')
        out = f'{path}.{at:g}s.png'
        video.frame(path, at, out)
        print(out)
