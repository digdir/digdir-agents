"""Recording the whole desktop for someone to watch.

`desktop record start` captures the display in the background, lightly compressed so that capturing keeps up, until
`desktop record stop` ends it and renders the video at the width asked for. The raw capture and its log are kept beside
the video until it is rendered.
"""

import fcntl
import os
import shutil
import signal
import subprocess
import time

from .session import DesktopError

FPS = 25
USAGE = 'record: takes start OUT.mp4 or stop [--width PIXELS]'


def alive(pid):
    try:
        # A recording started and stopped by the same process, as in a batch, is its child, and lingers as a zombie
        # that answers signals until it is reaped.
        os.waitpid(pid, os.WNOHANG)
    except ChildProcessError:
        pass
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # Another user's process, which has taken the number of a recorder that ended without `record stop`.
        return False
    return True


def read_state(session):
    """The running recording, as (ffmpeg pid, display width, display height, video), or None."""
    try:
        with open(session.recording_state) as f:
            ffmpeg, width, height, out = f.read().rstrip('\n').split(' ', 3)
    except (FileNotFoundError, ValueError):
        return None
    return int(ffmpeg), int(width), int(height), out


def running(session):
    """Whether a recording is running."""
    state = read_state(session)
    return bool(state and alive(state[0]))


def parts_dir(out):
    """Where the raw capture and its log are kept until the recording is rendered."""
    return os.path.splitext(out)[0] + '.parts'


def record(session, args):
    if args[:1] == ['start'] and len(args) == 2:
        start(session, args[1])
    elif args == ['stop']:
        stop(session, 1280)
    elif args[:2] == ['stop', '--width'] and len(args) == 3 and args[2].isdigit():
        stop(session, int(args[2]))
    else:
        raise DesktopError(USAGE)


def start(session, out):
    if not out.endswith('.mp4'):
        raise DesktopError(f'record: the recording must be an .mp4 file, got {out!r}')
    os.makedirs(session.captures, exist_ok=True)
    # Held until the recording is running, so that of two starts at once only one goes on, rather than both starting
    # a recorder and one of them losing track of it.
    with open(session.recording_state + '.lock', 'w') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise DesktopError('record: another recording is starting') from None
        begin(session, out)


def begin(session, out):
    state = read_state(session)
    if state and alive(state[0]):
        raise DesktopError('record: a recording is already running; stop it with `desktop record stop`')
    out = os.path.abspath(out)
    parts = parts_dir(out)
    shutil.rmtree(parts, ignore_errors=True)
    os.makedirs(parts)
    observed = session.geometry()
    width, height = observed.width, observed.height
    with open(os.path.join(parts, 'ffmpeg.log'), 'w') as log:
        ffmpeg = subprocess.Popen(
            ['ffmpeg', '-hide_banner', '-y', '-f', 'x11grab', '-framerate', str(FPS),
             '-video_size', f'{width}x{height}', '-draw_mouse', '1', '-i', session.display,
             '-c:v', 'libx264', '-preset', 'ultrafast', '-crf', '10', os.path.join(parts, 'raw.mkv')],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=log, start_new_session=True,
            env=session.tool_environ)
    # It runs in a session of its own, so that it outlives this command, and only the state file can stop it.
    try:
        with open(session.recording_state + '.new', 'w') as f:
            f.write(f'{ffmpeg.pid} {width} {height} {out}\n')
        os.replace(session.recording_state + '.new', session.recording_state)
    except OSError:
        ffmpeg.kill()
        raise
    # The grabber needs a moment before its first frame, and the first action should be in the recording.
    time.sleep(1)
    if ffmpeg.poll() is not None:
        os.remove(session.recording_state)
        raise DesktopError(f'record: ffmpeg could not record the display; see {os.path.join(parts, "ffmpeg.log")}')
    print(f'recording the {width}x{height} display into {out}; `desktop record stop` ends it')


def stop(session, out_width):
    state = read_state(session)
    if not state:
        raise DesktopError('record: no recording is running; start one with `desktop record start OUT.mp4`')
    ffmpeg, width, height, out = state
    # Claiming the state file first lets only one of two concurrent stops go on.
    claimed = session.recording_state + '.stopping'
    try:
        os.rename(session.recording_state, claimed)
    except FileNotFoundError:
        raise DesktopError('record: no recording is running; start one with `desktop record start OUT.mp4`') from None
    try:
        # Hold the last frame, so that the result stays on screen once the clip ends.
        time.sleep(0.5)
        if not alive(ffmpeg):
            raise DesktopError('record: the recording had already ended; see '
                               f'{os.path.join(parts_dir(out), "ffmpeg.log")}')
        os.kill(ffmpeg, signal.SIGINT)
        for _ in range(100):
            if not alive(ffmpeg):
                break
            time.sleep(0.1)
        else:
            raise DesktopError('record: ffmpeg did not finish the recording')
    finally:
        os.remove(claimed)
    render(parts_dir(out), out, width, height, out_width)
    shutil.rmtree(parts_dir(out))
    duration = probe_duration(out)
    print(f'{out}: {duration:.1f} s, {os.path.getsize(out)} bytes')


def probe_duration(path):
    result = subprocess.run(['ffprobe', '-v', 'error', '-show_entries', 'format=duration', '-of', 'csv=p=0', path],
                            capture_output=True, text=True)
    try:
        return float(result.stdout)
    except ValueError:
        raise DesktopError(f'record: cannot read the length of {path}: {result.stderr.strip()}') from None


def render(parts, out, width, height, out_width):
    out_width -= out_width % 2
    out_height = round(out_width * height / width / 2) * 2
    result = subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', '-i', os.path.join(parts, 'raw.mkv'),
                             '-vf', f'scale={out_width}:{out_height}:flags=lanczos,format=yuv420p', '-c:v', 'libx264',
                             '-crf', '23', '-preset', 'slow', '-movflags', '+faststart', '-an', out],
                            capture_output=True, text=True)
    if result.returncode != 0:
        raise DesktopError(f'record: cannot render {out}: {result.stderr.strip()[-500:]}')
