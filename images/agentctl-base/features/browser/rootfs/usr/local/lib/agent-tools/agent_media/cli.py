"""The command line."""

import re
import sys

from . import check
from .video import MediaError

USAGE = '''media <command> FILE...

  check FILE...         print each file's size, and for a clip its length and when it holds still long enough to
                        read, and write FILE.preview.png: the last frame of the last of those times, which shows the
                        result, and of the five longest others, or for a clip rarely still, evenly spaced frames up to
                        that result
  frames FILE TIME...   write the frame at each TIME, in seconds, to FILE.TIMEs.png at full size'''


def seconds(value):
    """A time in seconds, as `media frames` takes it."""
    if not re.fullmatch(r'[0-9]+(\.[0-9]+)?', value):
        raise MediaError(f"a time is a number of seconds, such as 12.5, got '{value}'", status=64)
    return float(value)


def main(argv):
    command, args = (argv[0], argv[1:]) if argv else ('', [])
    if command in ('help', '--help', '-h'):
        print(USAGE)
        return 0
    try:
        if command == 'check' and args:
            return run(args, check.check)
        if command == 'frames' and len(args) >= 2:
            times = [seconds(value) for value in args[1:]]
            return run(args[:1], lambda path: check.frames(path, times))
    except MediaError as error:
        print(f'media: {error}', file=sys.stderr)
        return error.status
    except KeyboardInterrupt:
        return 130
    if command not in ('', 'check', 'frames'):
        print(f"media: unknown command '{command}'", file=sys.stderr)
    print(USAGE, file=sys.stderr)
    return 64


def run(paths, action):
    """Runs ACTION on each of PATHS, going on past one that fails, and returns the status of the last failure."""
    status = 0
    for path in paths:
        try:
            action(path)
        except MediaError as error:
            print(f'media: {error}', file=sys.stderr)
            status = error.status
    return status
