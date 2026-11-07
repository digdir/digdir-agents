"""Recording the whole desktop for someone to watch.

`desktop record start` captures the display in the background, lightly compressed so that capturing keeps up, until
`desktop record stop` ends it, encodes the video at the width asked for and checks it with `media check`. Until the
video is encoded, the raw capture and its log are kept beside it and the state file names the recording, so that a
stop that failed or was interrupted can be run again. The video is encoded beside the capture and then moved into
place, so that a stop that was interrupted never leaves part of one where the video belongs.
"""

import contextlib
import fcntl
import os
import shutil
import signal
import subprocess
import sys
import time

from .session import DesktopError

FPS = 25
# What a recording keeps beside its video until `record stop` has rendered it.
RAW, LOG, ENCODED = 'raw.mkv', 'ffmpeg.log', 'encoded.mp4'
# The signals that end a command. `record start` holds them while it starts the recorder and names it in the state
# file, so that no recorder is left running that nothing names and so nothing stops.
ENDING = {signal.SIGINT, signal.SIGTERM, signal.SIGHUP}
USAGE = 'record: takes start OUT.mp4 or stop [--width PIXELS], at least 160 pixels wide'


def alive(pid):
    """Whether process PID is running. A process that has ended but not been reaped, a zombie, has ended: a recording
    started in a batch is the batch's child, and lingers as a zombie while the batch runs."""
    try:
        # Reaped here when it is this process's child, as when a batch starts and stops it.
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
    try:
        with open(f'/proc/{pid}/stat') as f:
            # The state comes after the command name, which is in parentheses and can hold any character.
            return f.read().rsplit(')', 1)[1].split()[0] != 'Z'
    except (OSError, IndexError):
        return False


def recorder(pid, out):
    """Whether PID is still the ffmpeg recording OUT. Process numbers are reused soon after a process ends, so a
    recorder that ended without `record stop` can leave its number to another process, which must not be signalled."""
    if not alive(pid):
        return False
    try:
        with open(f'/proc/{pid}/cmdline', 'rb') as f:
            command = f.read().split(b'\0')
    except OSError:
        return False
    return os.fsencode(os.path.join(parts_dir(out), RAW)) in command


def recorded(out):
    """Whether any process is a recorder of OUT, such as one that another display's `record start` started."""
    return any(recorder(int(pid), out) for pid in os.listdir('/proc') if pid.isdigit())


def read_state(session):
    """The recording, running or waiting for `record stop` to render it, as (ffmpeg pid, video), or None."""
    try:
        with open(session.recording_state) as f:
            ffmpeg, out = f.read().rstrip('\n').split(' ', 1)
        return int(ffmpeg), out
    except (FileNotFoundError, ValueError):
        return None


def running(session):
    """Whether a recording is running."""
    state = read_state(session)
    return bool(state and recorder(*state))


def parts_dir(out):
    """Where the raw capture and its log are kept until the recording is rendered."""
    return os.path.splitext(out)[0] + '.parts'


def ours(parts):
    """Whether PARTS holds only what a recording leaves there, and so can be removed."""
    return set(os.listdir(parts)) <= {RAW, LOG, ENCODED}


def record(session, args):
    if args[:1] == ['start'] and len(args) == 2:
        start(session, args[1])
    elif args == ['stop']:
        stop(session, 1280)
    elif args[:2] == ['stop', '--width'] and len(args) == 3 and args[2].isdigit() and int(args[2]) >= 160:
        stop(session, int(args[2]))
    else:
        raise DesktopError(USAGE)


@contextlib.contextmanager
def claimed(session):
    """Held while `record start` or `record stop` runs, so that of two at once only one goes on, rather than both
    starting a recorder, or both stopping and encoding one. The system lets it go when the command ends however it
    ends."""
    os.makedirs(session.captures, exist_ok=True)
    with open(session.recording_state + '.lock', 'w') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise DesktopError('record: another `desktop record start` or `stop` is running') from None
        yield


def start(session, out):
    if not out.endswith('.mp4'):
        raise DesktopError(f'record: the recording must be an .mp4 file, got {out!r}')
    with claimed(session):
        begin(session, out)


def begin(session, out):
    try:
        out.encode()
    except UnicodeEncodeError:
        raise DesktopError(f"record: the recording's path has to be UTF-8, got {out!r}") from None
    state = read_state(session)
    if state and recorder(*state):
        raise DesktopError('record: a recording is already running; stop it with `desktop record stop`')
    out = os.path.abspath(out)
    parts = parts_dir(out)
    if recorded(out):
        raise DesktopError(f'record: another recording is writing to {os.path.join(parts, RAW)}; '
                           'record to another file')
    # This display's own recording of OUT that no stop rendered is replaced, as discard says. Any other capture there
    # is another display's, or one whose state is gone, which is not for this command to throw away.
    if os.path.isdir(parts) and not (state and state[1] == out) and os.path.exists(os.path.join(parts, RAW)):
        raise DesktopError(f'record: {parts} holds a capture that `desktop record stop` has not rendered; render it on '
                           'the display that recorded it, or remove it, or record to another file')
    if os.path.isdir(parts) and not ours(parts):
        raise DesktopError(f'record: {parts} is in the way; remove it or record to another file')
    observed = session.geometry()
    width, height = observed.width, observed.height
    if state:
        discard(session, state[1])
    # What is left is the log of a recorder that could not start.
    shutil.rmtree(parts, ignore_errors=True)
    os.makedirs(os.path.dirname(parts), exist_ok=True)
    try:
        os.mkdir(parts)
    except FileExistsError:
        raise DesktopError(f'record: {parts} is in the way; remove it or record to another file') from None
    unheld = signal.pthread_sigmask(signal.SIG_BLOCK, ENDING)
    try:
        with open(os.path.join(parts, LOG), 'w') as log:
            # It runs in a session of its own, so that it outlives this command, and only the state file can stop it.
            ffmpeg = subprocess.Popen(
                ['ffmpeg', '-hide_banner', '-y', '-f', 'x11grab', '-framerate', str(FPS),
                 '-video_size', f'{width}x{height}', '-draw_mouse', '1', '-i', session.display,
                 '-c:v', 'libx264', '-preset', 'ultrafast', '-crf', '10', os.path.join(parts, RAW)],
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=log, start_new_session=True,
                env=session.tool_environ, preexec_fn=lambda: signal.pthread_sigmask(signal.SIG_SETMASK, unheld))
        try:
            with open(session.recording_state + '.new', 'w') as f:
                f.write(f'{ffmpeg.pid} {out}\n')
            os.replace(session.recording_state + '.new', session.recording_state)
            # A signal that came meanwhile ends this command here, and with it the recorder.
            signal.pthread_sigmask(signal.SIG_SETMASK, unheld)
            # The grabber needs a moment before its first frame, and the first action should be in the recording.
            time.sleep(1)
            if ffmpeg.poll() is not None:
                raise DesktopError(f'record: ffmpeg could not record the display; see {os.path.join(parts, LOG)}')
        except BaseException:
            # A recording that `record start` did not report started is not left running, since nothing would stop it.
            ffmpeg.kill()
            ffmpeg.wait()
            with contextlib.suppress(FileNotFoundError):
                os.remove(session.recording_state)
            raise
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, unheld)
    print(f'recording the {width}x{height} display into {out}; `desktop record stop` ends it')


def discard(session, out):
    """Lets go of the recording of OUT, whose recorder has ended, and of its capture if `record stop` never rendered
    it, saying so, since that is a take lost. A capture beside files that are not the recording's is left where it
    is."""
    parts = parts_dir(out)
    if os.path.exists(os.path.join(parts, RAW)):
        if ours(parts):
            shutil.rmtree(parts)
            print(f'desktop: record: discarding the recording of {out}, which `desktop record stop` would have '
                  'rendered', file=sys.stderr)
        else:
            print(f'desktop: record: letting go of the recording of {out} without rendering it; its capture is left '
                  f'in {parts}, which holds other files too', file=sys.stderr)
    os.remove(session.recording_state)


def stop(session, out_width):
    with claimed(session):
        state = read_state(session)
        if not state:
            raise DesktopError('record: no recording is running; start one with `desktop record start OUT.mp4`')
        ffmpeg, out = state
        parts = parts_dir(out)
        raw, log, encoded = (os.path.join(parts, name) for name in (RAW, LOG, ENCODED))
        if recorder(ffmpeg, out):
            # Hold the last frame, so that the result stays on screen once the clip ends.
            time.sleep(0.5)
            finish(ffmpeg)
        elif os.path.exists(raw):
            # Its recorder has ended, by an earlier stop that then failed or was interrupted, or on its own.
            print('desktop: record: the recording had already ended; rendering what it captured', file=sys.stderr)
        else:
            os.remove(session.recording_state)
            raise DesktopError('record: the recording had already ended and left nothing to render'
                               + (f'; see {log}' if os.path.exists(log) else ''))
        # The media command's, so that a recording is encoded and checked as any other clip is.
        from agent_media import check, video
        kept = 'Its capture is kept for `desktop record stop` to try again, and `desktop record start` discards it.'
        try:
            video.encode(raw, encoded, out_width)
        except video.MediaError as error:
            raise DesktopError(f'record: cannot encode {out}: {error}\n{kept}') from None
        try:
            os.replace(encoded, out)
        except OSError as error:
            raise DesktopError(f'record: cannot write {out}: {error.strerror}\n{kept}') from None
        os.remove(session.recording_state)
        shutil.rmtree(parts)
    try:
        check.check(out)
    except video.MediaError as error:
        raise DesktopError(f'record: {error}') from None


def finish(ffmpeg):
    """Ends the recorder, letting ffmpeg close the file it writes, or after 10 s ending it outright, which leaves a
    capture that can still be encoded."""
    for ending, polls in ((signal.SIGINT, 100), (signal.SIGKILL, 20)):
        try:
            os.kill(ffmpeg, ending)
        except ProcessLookupError:
            # It ended, and was reaped, since it was checked.
            return
        for _ in range(polls):
            if not alive(ffmpeg):
                return
            time.sleep(0.1)
    raise DesktopError('record: ffmpeg did not end')
