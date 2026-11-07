"""Typing text and pressing keys."""

import fcntl
import os
import re
import subprocess

from . import record
from .session import DesktopError, integer

# xdotool's delay between typed characters, in milliseconds, and while recording, slow enough that someone watching can
# read along. xdotool types a character in about half of it.
TYPE_DELAY = 12
RECORDING_TYPE_DELAY = 180


def keysym(character):
    """The keysym X knows a character by: Latin-1 as itself, the rest of Unicode offset by 0x1000000."""
    code = ord(character)
    return code if code <= 0xff else code | 0x1000000


def plan(text, present, spare):
    """Splits TEXT into segments, each with the spare keycodes to bind first, as (segment, [(keycode, keysym)]).

    xdotool binds a character the layout lacks to one level of a spare keycode, which X expands to a lower and
    upper pair, so ÆØÅ would arrive as æøå. Binding such characters to both levels first keeps their case. xdotool
    binds one spare keycode itself, and the text is typed in segments when it lacks more characters than there are
    left.
    """
    capacity = len(spare) - 1
    segments, segment, bindings, bound = [], '', [], set()
    for character in text:
        code = ord(character)
        # xdotool maps control characters itself.
        if code >= 0x20 and (code < 0x7f or code >= 0xa0) and capacity > 0:
            symbol = keysym(character)
            if symbol not in present and symbol not in bound:
                if len(bindings) == capacity:
                    segments.append((segment, bindings))
                    segment, bindings, bound = '', [], set()
                bindings.append((spare[len(bindings)], symbol))
                bound.add(symbol)
        segment += character
    segments.append((segment, bindings))
    return segments


def keymap(output):
    """The keysyms a keyboard map has, and its unmapped keycodes, from `xmodmap -pk`."""
    present = {int(symbol, 16) for symbol in re.findall(r'0x[0-9a-f]+', output)}
    spare = [fields[0] for fields in (line.split() for line in output.splitlines())
             if len(fields) == 1 and fields[0].isdigit()]
    return present, spare


def type_text(session, args):
    if not args:
        raise DesktopError('type takes the text to enter')
    text = ' '.join(args)
    delay = RECORDING_TYPE_DELAY if record.running(session) else TYPE_DELAY
    os.makedirs(session.captures, exist_ok=True)
    # Concurrent types would pick the same spare keycodes.
    with open(os.path.join(session.captures, '.type.lock'), 'w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        try:
            output = session.run('xmodmap', '-pk')
        except DesktopError:
            raise DesktopError(f'cannot read the keyboard map of display {session.display}') from None
        for segment, bindings in plan(text, *keymap(output)):
            type_bound(session, segment, bindings, delay)
    session.sync()


def type_bound(session, text, bindings, delay):
    """Types TEXT with BINDINGS in place. An interrupted type leaves no key bound and no xdotool typing."""
    bind, unbind = [], []
    for keycode, symbol in bindings:
        bind += ['-e', f'keycode {keycode} = 0x{symbol:x} 0x{symbol:x}']
        unbind += ['-e', f'keycode {keycode} =']
    typing = None
    # From the binding on, so that a signal that arrives while xmodmap binds still unbinds.
    try:
        if bind:
            try:
                session.run('xmodmap', *bind)
            except DesktopError:
                raise DesktopError(f'cannot bind keys on display {session.display}') from None
        typing = subprocess.Popen(['xdotool', 'type', '--delay', str(delay), '--', text], env=session.tool_environ,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        _, error = typing.communicate()
        if typing.returncode != 0:
            raise DesktopError(f'xdotool type: {error.strip() or typing.returncode}')
    finally:
        if typing and typing.poll() is None:
            typing.kill()
        if unbind:
            session.run('xmodmap', *unbind, check=False)


def key(session, args):
    repeat, keys = '1', []
    while args:
        if args[0] == '--repeat':
            if len(args) < 2 or not args[1]:
                raise DesktopError('--repeat needs a count')
            repeat, args = args[1], args[2:]
        else:
            keys.append(args[0])
            args = args[1:]
    if not keys:
        raise DesktopError('key takes an X11 key combination such as ctrl+s or Page_Down')
    integer(repeat, '--repeat')
    # --clearmodifiers releases modifiers a person or a stray keydown left pressed, but under hold-key the held
    # modifier is the point, so it is kept.
    clear = [] if session.holding else ['--clearmodifiers']
    session.run('xdotool', 'key', *clear, '--repeat', repeat, '--delay', '40', '--', *keys)
    session.sync()


def hold_key(session, args, run):
    """Holds a key while RUN runs the rest of ARGS as another command."""
    if len(args) < 2:
        raise DesktopError('hold-key takes a keysym and a command')
    held = args[0]
    session.run('xdotool', 'keydown', '--', held)
    outer, session.holding = session.holding, held
    try:
        return run(args[1:])
    finally:
        session.holding = outer
        session.run('xdotool', 'keyup', '--', held, check=False)
        session.sync()
