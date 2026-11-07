"""Screenshots, settled before they are written, so that a screenshot never shows a half-drawn frame."""

import json
import os
import random
import subprocess
import time

from .geometry import scale
from .session import DesktopError, integer


def require_jpeg(path):
    """Captures are JPEG: the settle loop takes two byte-identical frames as proof that the screen has stopped
    changing, and that only holds for one deterministic encoder."""
    if not path.endswith(('.jpg', '.jpeg')):
        raise DesktopError(f"--out must name a .jpg file, got '{path}'")


def options(command, args):
    """Parses `--out PATH`, the only option a capture takes."""
    out = ''
    while args:
        if args[0] != '--out':
            raise DesktopError(f"{command}: unexpected argument '{args[0]}'")
        if len(args) < 2 or not args[1]:
            raise DesktopError('--out needs a path')
        require_jpeg(args[1])
        out, args = args[1], args[2:]
    return out


def grab(session, output, size, offset, scaled=None):
    command = ['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', '-f', 'x11grab', '-draw_mouse', '1',
               '-video_size', size, '-i', f'{session.display}.0+{offset}', '-frames:v', '1']
    if scaled:
        command += ['-vf', f'scale={scaled[0]}:{scaled[1]}:flags=bilinear']
    command += ['-q:v', str(session.jpeg_quality), output]
    if subprocess.run(command, capture_output=True).returncode != 0:
        raise DesktopError(f'cannot capture display {session.display}')


def settled(session, output, size, offset, scaled=None):
    """Captures until two consecutive frames are identical, and returns how many frames that took."""
    scratch = os.path.splitext(output)[0] + '.settling.jpg'
    previous = None
    for attempt in range(1, session.settle_attempts + 1):
        grab(session, scratch, size, offset, scaled)
        with open(scratch, 'rb') as f:
            current = f.read()
        if current == previous:
            break
        previous = current
        if attempt < session.settle_attempts:
            time.sleep(session.settle_interval)
    os.replace(scratch, output)
    return attempt


def new_path(session, prefix):
    os.makedirs(session.captures, exist_ok=True)
    # Captures share /tmp with builds. Keep recent captures readable and let the rest go, so that a long session, or a
    # loop that screenshots more than it should, cannot fill it.
    captures = sorted((os.path.join(session.captures, name) for name in os.listdir(session.captures)
                       if name.endswith('.jpg')), key=modified, reverse=True)
    for old in captures[session.keep_captures - 1:]:
        try:
            os.remove(old)
        except FileNotFoundError:
            pass
    stamp = time.strftime('%Y%m%d-%H%M%S')
    return os.path.join(session.captures, f'{prefix}-{stamp}-{random.randrange(10000):04d}.jpg')


def modified(path):
    try:
        return os.path.getmtime(path)
    except FileNotFoundError:
        return 0


def report(session, path, width, height, frames, note=''):
    size = os.path.getsize(path)
    if session.json:
        print(json.dumps({'path': path, 'width': width, 'height': height, 'frames': frames, 'bytes': size},
                         separators=(',', ':'), ensure_ascii=False))
    else:
        print(f'{path} ({width}x{height}, {size} bytes){note}')


def screenshot(session, args):
    out = options('screenshot', args)
    observed = session.geometry()
    out = out or new_path(session, 'screen')
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
    size = f'{observed.width}x{observed.height}'
    note = ''
    if observed.sent_width != observed.width:
        frames = settled(session, out, size, '0,0', (observed.sent_width, observed.sent_height))
        note = (f' — display is {observed.width}x{observed.height}; coordinates are in these '
                f'{observed.sent_width}x{observed.sent_height} pixels')
    else:
        frames = settled(session, out, size, '0,0')
    session.record_observed(observed)
    report(session, out, observed.sent_width, observed.sent_height, frames, note)


def zoom(session, args):
    if len(args) < 4:
        raise DesktopError('zoom takes X Y W H')
    x, y, w, h = (integer(value, f'zoom {name}') for value, name in zip(args[:4], 'xywh'))
    out = options('zoom', args[4:])
    addressed = session.addressed()
    x, w = (scale(v, addressed.sent_width, addressed.width) for v in (x, w))
    y, h = (scale(v, addressed.sent_height, addressed.height) for v in (y, h))
    if not (0 <= x < addressed.width and 0 <= y < addressed.height and w > 0 and h > 0):
        raise DesktopError('zoom: region must start inside the display')
    w, h = min(w, addressed.width - x), min(h, addressed.height - y)
    out = out or new_path(session, 'zoom')
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
    frames = settled(session, out, f'{w}x{h}', f'{x},{y}')
    report(session, out, w, h, frames, ' — region at native resolution')
