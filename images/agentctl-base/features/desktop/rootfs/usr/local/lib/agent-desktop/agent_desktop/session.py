"""What every command shares: the environment's settings, the X tools and the screenshot geometry."""

import os
import re
import subprocess

from . import geometry


class DesktopError(Exception):
    """A command failed. The message is printed after `desktop: `."""


def setting(environ, name, default, kind):
    """A tuning setting, read when a command uses it, so that a bad one fails only the commands it affects."""
    value = environ.get(name) or default
    try:
        return kind(value)
    except ValueError:
        what = 'an integer' if kind is int else 'a number'
        raise DesktopError(f"{name} must be {what}, got '{value}'") from None


class Session:
    def __init__(self, environ, json=False):
        self.environ, self.json = environ, json
        # The key `hold-key` holds while it runs another command, which must then keep it pressed.
        self.holding = ''
        # The image points DISPLAY at the agent's own desktop. DESKTOP_DISPLAY overrides it for a second screen or a
        # test harness.
        self.display = environ.get('DESKTOP_DISPLAY') or environ.get('DISPLAY') or ':1'
        self.captures = environ.get('DESKTOP_CAPTURES') or '/tmp/desktop'
        # The geometry of the last screenshot or tree, which every coordinate refers to. One per session, since
        # sessions with different harnesses get screenshots at different sizes.
        session = environ.get('AGENT_SESSION_ID')
        self.observed = os.path.join(self.captures, '.screenshot-geometry' + (f'.{session}' if session else ''))
        # The running recording on this display, if any: one per display rather than per session.
        self.recording_state = os.path.join(self.captures, '.recording' + self.display)
        try:
            self.budget = geometry.budget(environ)
        except geometry.BudgetError as error:
            raise DesktopError(str(error)) from None
        # The tools run on this display. xdotool decodes the text it types with the locale's character set and
        # refuses a multi-byte sequence under the POSIX locale, so without a UTF-8 locale `type` cannot enter æ.
        self.tool_environ = dict(environ, DISPLAY=self.display, LC_ALL=environ.get('LC_ALL') or 'C.UTF-8')

    # ffmpeg's mjpeg scale: 7 encodes at JPEG quality 75, the quality Anthropic's reference implementation sends. Lower
    # numbers are better quality and larger files.
    @property
    def jpeg_quality(self):
        return setting(self.environ, 'DESKTOP_JPEG_QUALITY', '7', int)

    # A capture is settled once two consecutive frames are byte-identical. mjpeg output is deterministic, so the second
    # identical frame is itself the screenshot and costs nothing extra.
    @property
    def settle_attempts(self):
        return setting(self.environ, 'DESKTOP_SETTLE_ATTEMPTS', '8', int)

    @property
    def settle_interval(self):
        return setting(self.environ, 'DESKTOP_SETTLE_INTERVAL', '0.1', float)

    @property
    def keep_captures(self):
        return setting(self.environ, 'DESKTOP_KEEP_CAPTURES', '200', int)

    def run(self, *command, check=True):
        """Runs an X tool and returns its output. A failure becomes a DesktopError that carries the tool's message."""
        result = subprocess.run(command, capture_output=True, text=True, env=self.tool_environ)
        if check and result.returncode != 0:
            raise DesktopError(f'{command[0]}: {result.stderr.strip() or f"exited with status {result.returncode}"}')
        return result.stdout

    def succeeds(self, *command):
        return subprocess.run(command, capture_output=True, env=self.tool_environ).returncode == 0

    def sync(self):
        """Blocks until the X server has processed everything sent before it, so the next command does not race the
        one before it. A request that needs a reply is a round trip by definition."""
        self.run('xdotool', 'getactivewindow', check=False)

    # --- geometry ---------------------------------------------------------------------------------------------------

    def geometry(self):
        """The display's size and the size screenshots of it are sent at."""
        try:
            info = self.run('xdpyinfo')
        except (DesktopError, FileNotFoundError):
            raise DesktopError(f'cannot read display {self.display}') from None
        match = re.search(r'dimensions:\s+(\d+)x(\d+)', info)
        if not match:
            raise DesktopError(f'cannot read display {self.display}')
        return geometry.fit(int(match.group(1)), int(match.group(2)), self.budget)

    def record_observed(self, observed):
        """Records the geometry an observation's pixels are in, for `addressed` to hold coordinates to. Written whole,
        so that a concurrent command never reads half a record."""
        os.makedirs(self.captures, exist_ok=True)
        scratch = f'{self.observed}.{os.getpid()}'
        with open(scratch, 'w') as f:
            f.write(f'{observed.width} {observed.height} {observed.sent_width} {observed.sent_height}\n')
        os.replace(scratch, self.observed)

    def forget_observed(self):
        try:
            os.remove(self.observed)
        except FileNotFoundError:
            pass

    def addressed(self):
        """The geometry coordinates refer to: the last observation's, or the display's when there has been none.
        Refuses when the display has changed size since that observation, because its coordinates would then
        address somewhere else."""
        current = self.geometry()
        try:
            with open(self.observed) as f:
                width, height, sent_width, sent_height = (int(v) for v in f.read().split())
        except FileNotFoundError:
            return current
        if (width, height) != (current.width, current.height):
            raise DesktopError(f'the display changed from {width}x{height} to {current.width}x{current.height} '
                               'since the last screenshot or tree; take a new one')
        return geometry.Geometry(width, height, sent_width, sent_height)


def integer(value, what):
    if not re.fullmatch(r'-?[0-9]+', value or ''):
        raise DesktopError(f"{what} must be an integer, got '{value}'")
    return int(value)

