"""The command line: parsing, dispatch and batches."""

import contextlib
import importlib
import io
import os
import signal
import sys

from .session import DesktopError, Session

USAGE = '''desktop <command> [arguments]

Observe
  screenshot [--out PATH]        capture the display, settled, within the screenshot budget
  zoom X Y W H [--out PATH]      capture one region at native resolution
  windows                        list mapped windows with geometry, class and title
  tree [APPLICATION]             what is showing, as an accessibility tree with click boxes
  cursor                         print the pointer position
  display                        print display geometry, the screenshot budget and its source, the keyboard layout

Point
  move X Y                       move the pointer
  click [X Y]                    left click, optionally after moving
  right-click [X Y]              right click
  middle-click [X Y]             middle click
  double-click [X Y]             double click
  triple-click [X Y]             triple click
  mouse-down [BUTTON]            press and hold a button (default 1)
  mouse-up [BUTTON]              release a button (default 1)
  drag X1 Y1 X2 Y2               press at the first point, move, release at the second
  scroll up|down|left|right [N]  scroll N steps (default 3) at the pointer

Type
  type TEXT                      type literal text, any Unicode, whatever the keyboard layout
  key KEYSYM [--repeat N]        press an X11 key combination, for example ctrl+s or Page_Down
  hold-key KEYSYM COMMAND...     hold a modifier while running another desktop command

Windows and session
  focus WINDOW                   raise and focus a window by id or title substring
  resize WIDTH HEIGHT            resize the display with RandR
  wait SECONDS                   pause

Record
  record start OUT.mp4           record the display until `record stop`; meanwhile the pointer glides and typing slows
  record stop [--width PIXELS]   stop and render OUT.mp4, 1280 px wide by default

Batch
  batch [--no-screenshot]        read one command per line from stdin and run them in order

Options
  --json                         machine-readable output, where a command supports it'''

# Each command's module and function, and the arguments that come before the command line's. A command imports only
# its own module, so that commands start quickly.
COMMANDS = {
    'screenshot': ('capture', 'screenshot'),
    'zoom': ('capture', 'zoom'),
    'windows': ('windows', 'windows'),
    'tree': ('tree', 'tree'),
    'cursor': ('pointer', 'cursor'),
    'display': ('windows', 'display'),
    'move': ('pointer', 'move'),
    'click': ('pointer', 'click', 1),
    'right-click': ('pointer', 'click', 3),
    'middle-click': ('pointer', 'click', 2),
    'double-click': ('pointer', 'click', 'double'),
    'triple-click': ('pointer', 'click', 'triple'),
    'mouse-down': ('pointer', 'button', 'mousedown'),
    'mouse-up': ('pointer', 'button', 'mouseup'),
    'drag': ('pointer', 'drag'),
    'scroll': ('pointer', 'scroll'),
    'type': ('keyboard', 'type_text'),
    'key': ('keyboard', 'key'),
    'hold-key': ('keyboard', 'hold_key'),
    'focus': ('windows', 'focus'),
    'resize': ('windows', 'resize'),
    'wait': ('windows', 'wait'),
    'record': ('record', 'record'),
    'batch': ('cli', 'batch'),
}


def dispatch(session, args):
    """Runs one command, and returns its exit status. A failure raises DesktopError."""
    command, args = args[0], args[1:]
    if command in ('help', '--help', '-h'):
        print(USAGE)
        return 0
    if command not in COMMANDS:
        raise DesktopError(f"unknown command '{command}'; run 'desktop help'")
    module, function, *before = COMMANDS[command]
    run = getattr(importlib.import_module(f'agent_desktop.{module}'), function)
    if command == 'hold-key':
        return run(session, args, lambda rest: dispatch(session, rest)) or 0
    if command == 'batch':
        return run(session, args, sys.stdin) or 0
    return run(session, *before, args) or 0


def batch_line(line):
    """The command a batch line holds, or None for a blank line or a comment.

    A batch line is split on whitespace, with no quoting and no shell expansion: text a batch line carries can come
    from the screen, and the screen is untrusted. The two commands whose last argument is free text take the rest of
    the line verbatim instead, so text with spaces needs no quotes.
    """
    words = line.split()
    if not words or words[0].startswith('#'):
        return None
    if words[0] in ('type', 'focus'):
        rest = line[line.index(words[0]) + len(words[0]):]
        rest = rest[1:] if rest.startswith(' ') else rest
        return [words[0], rest] if rest else [words[0]]
    return words


def batch(session, args, lines, run=None):
    """Runs one command per line, stopping at the first that fails, and ends with a screenshot unless told not to."""
    screenshot = True
    for arg in args:
        if arg != '--no-screenshot':
            raise DesktopError(f"batch: unexpected argument '{arg}'")
        screenshot = False
    run = run or (lambda words: dispatch(session, words))
    failed = False
    # Only a newline ends a line. Other line breaks, such as U+2028, are text a `type` line carries.
    text = lines.read()
    for index, line in enumerate(text.split('\n')[:-1] if text.endswith('\n') else text.split('\n'), 1):
        words = batch_line(line)
        if words is None:
            continue
        print(f'{index:2d} {line}')
        if failed:
            print('   Not executed: an earlier computer action in this turn failed.')
            continue
        output = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
            try:
                failed = bool(run(words))
            except Exception as error:
                print(f'desktop: {describe(error)}', file=sys.stderr)
                failed = True
        for printed in output.getvalue().split('\n')[:-1]:
            print(f'   {printed}')
    if screenshot:
        from .capture import screenshot as take
        take(session, [])
    return 1 if failed else 0


def describe(error):
    return str(error) if isinstance(error, DesktopError) else f'{type(error).__name__}: {error}'


def terminate(signum, frame):
    """Ends the command as an exception would, so that it releases what it holds, such as a key binding or a mouse
    button, on the way out."""
    raise SystemExit(128 + signum)


def main(argv):
    # --json is a global option, recognized only before the command, so that `type --json` and a batch line carrying
    # the same text stay literal.
    as_json = bool(argv) and argv[0] == '--json'
    if as_json:
        argv = argv[1:]
    if not argv:
        print(USAGE)
        return 64
    # A batch prints each line's outcome as it goes, also into a pipe, as the bash version did.
    sys.stdout.reconfigure(line_buffering=True)
    signal.signal(signal.SIGTERM, terminate)
    signal.signal(signal.SIGHUP, terminate)
    try:
        session = Session(dict(os.environ), json=as_json)
        # In this process too, for the accessibility libraries `desktop tree` loads.
        os.environ.update(DISPLAY=session.display, LC_ALL=session.tool_environ['LC_ALL'])
        return dispatch(session, argv)
    except KeyboardInterrupt:
        return 130
    except Exception as error:
        print(f'desktop: {describe(error)}', file=sys.stderr)
        return 1
