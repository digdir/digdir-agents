"""The video work, done by ffmpeg: reading a file's properties, measuring how its picture changes, taking frames out
of it and encoding it."""

import ctypes
import json
import os
import signal
import subprocess
import tempfile
from collections import namedtuple

# The size of a file, and of its picture, and its length in seconds, or None when the file does not say. CLIP is
# whether it moves, rather than being a single picture such as a PNG.
Probe = namedtuple('Probe', 'width height duration size clip')


class MediaError(Exception):
    """A file cannot be read or worked on. STATUS is the exit status `media` ends with: 66 for a file it cannot open,
    65 for one ffmpeg cannot read."""

    def __init__(self, message, status=65):
        super().__init__(message)
        self.status = status


# prctl's option that has the kernel signal a process when the one that started it ends.
PR_SET_PDEATHSIG = 1
LIBC = ctypes.CDLL(None, use_errno=True)


def ffmpeg(*args):
    """Runs ffmpeg, and returns its output. A failure raises MediaError with the end of its message.

    ffmpeg ends with the command that runs it, however that ends, rather than encoding on for no one, possibly into a
    file that a rerun of the command writes too."""
    parent = os.getpid()

    def end_with_parent():
        LIBC.prctl(PR_SET_PDEATHSIG, signal.SIGKILL)
        # It may have ended before then.
        if os.getppid() != parent:
            os._exit(1)

    result = subprocess.run(['ffmpeg', '-hide_banner', '-nostdin', '-y', *args], capture_output=True, text=True,
                            preexec_fn=end_with_parent)
    if result.returncode != 0:
        raise MediaError(f'ffmpeg: {result.stderr.strip()[-500:] or f"exited with status {result.returncode}"}')
    return result.stdout


def probe(path):
    if not os.access(path, os.R_OK) or os.path.isdir(path):
        raise MediaError(f'cannot read {path}', status=66)
    result = subprocess.run(['ffprobe', '-v', 'error', '-select_streams', 'v:0', '-show_entries',
                             'stream=width,height:format=format_name,duration,size', '-of', 'json', path],
                            capture_output=True, text=True)
    try:
        info = json.loads(result.stdout) if result.returncode == 0 else {}
        stream, container = info['streams'][0], info['format']
        width, height, size = int(stream['width']), int(stream['height']), int(container['size'])
    except (ValueError, KeyError, IndexError):
        raise MediaError(f'{path} is not a picture or video ffmpeg can read') from None
    duration = float(container['duration']) if 'duration' in container else None
    # ffmpeg reads a single picture with an image demuxer, whose names are these.
    name = container.get('format_name', '')
    clip = not (name == 'image2' or name.endswith('_pipe'))
    return Probe(width, height, duration, size, clip)


def differences(path, part, lag, rate):
    """How the picture changes, as (time, recent, older, previous) for each of RATE frames a second: the largest
    difference, out of 255, between any square PART pixels wide and the same square LAG seconds earlier, twice LAG
    seconds earlier and a frame earlier."""
    lags = {'recent': lag, 'older': 2 * lag, 'previous': 1 / rate}
    with tempfile.TemporaryDirectory() as scratch:
        # Each delayed copy runs past the end, so its comparison stops with the clip. Shrinking a difference by area
        # averages each square into one pixel, whose brightest is the largest change.
        graph = [f'[0:v]fps={rate},format=gray,split={2 * len(lags)}' + ''.join(f'[now_{name}][then_{name}]'
                                                                               for name in lags)]
        outputs = []
        for name, earlier in lags.items():
            graph += [f'[then_{name}]setpts=PTS+{earlier}/TB[then_{name}]',
                      f'[now_{name}][then_{name}]blend=all_mode=difference:shortest=1,'
                      f'scale=iw/{part}:ih/{part}:flags=area,format=gray,signalstats,'
                      f'metadata=print:key=lavfi.signalstats.YMAX:file={os.path.join(scratch, name)}[{name}]']
            outputs += ['-map', f'[{name}]', '-f', 'null', '-']
        ffmpeg('-nostats', '-i', path, '-an', '-filter_complex', ';'.join(graph), *outputs)
        changes = {}
        for name in lags:
            with open(os.path.join(scratch, name)) as f:
                changes[name] = dict(measured(f.read()))
    # Before the clip starts is black, so a frame missing from a comparison is as different as can be.
    return [(time, change, *(changes[name].get(time, 255) for name in ('older', 'previous')))
            for time, change in changes['recent'].items()]


def measured(output):
    """The (time, change) of each frame in what ffmpeg's metadata filter printed."""
    samples, time = [], None
    for line in output.splitlines():
        if line.startswith('frame:'):
            time = float(line.rsplit('pts_time:', 1)[1])
        elif line.startswith('lavfi.signalstats.YMAX=') and time is not None:
            samples.append((time, float(line.split('=', 1)[1])))
    return samples


def frame(path, at, out):
    """Writes the frame AT seconds into PATH to OUT, at full size."""
    ffmpeg('-loglevel', 'error', '-ss', f'{at:.3f}', '-i', path, '-frames:v', '1', out)
    if not os.path.exists(out):
        raise MediaError(f'{path} has no frame at {at:g} s')


def sheet(path, frames, out, width=800, columns=2):
    """Writes a contact sheet of FRAMES, (time, label) in PATH, to OUT: each frame WIDTH pixels wide, with its label in
    a strip above it rather than over the picture, in rows of COLUMNS."""
    with tempfile.TemporaryDirectory() as scratch:
        for index, (at, label) in enumerate(frames, 1):
            # The label is given as a file, so that its text needs no escaping for the filter.
            label_file = os.path.join(scratch, f'{index:03d}.txt')
            with open(label_file, 'w') as f:
                f.write(label)
            ffmpeg('-loglevel', 'error', '-ss', f'{at:.3f}', '-i', path, '-frames:v', '1', '-vf',
                   f"scale={width}:-2,pad=iw:ih+32:0:32:color=gray,"
                   f"drawtext=textfile='{label_file}':fontsize=22:fontcolor=white:x=8:y=5",
                   os.path.join(scratch, f'{index:03d}.png'))
        rows = (len(frames) + columns - 1) // columns
        ffmpeg('-loglevel', 'error', '-i', os.path.join(scratch, '%03d.png'), '-vf',
               f'tile={columns}x{rows}:padding=8:color=gray', '-frames:v', '1', out)


def scaled(width, height, most):
    """The size to encode a WIDTH x HEIGHT picture at to make it at most MOST pixels wide: never larger, and even,
    which H.264 in the yuv420p format browsers play requires."""
    out_width = min(most, width)
    out_width -= out_width % 2
    return out_width, round(out_width * height / width / 2) * 2


def encode(source, out, width):
    """Encodes SOURCE as an MP4 at most WIDTH pixels wide that browsers, and GitHub, play as it is."""
    out_width, out_height = scaled(*probe(source)[:2], width)
    ffmpeg('-loglevel', 'error', '-i', source, '-vf', f'scale={out_width}:{out_height}:flags=lanczos,format=yuv420p',
           '-c:v', 'libx264', '-crf', '23', '-preset', 'slow', '-movflags', '+faststart', '-an', out)
