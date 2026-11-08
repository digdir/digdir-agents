"""Pointing: moving the pointer, clicking, dragging and scrolling."""

import json
import math

from . import record
from .geometry import scale
from .session import DesktopError, integer

BUTTONS = {'up': 4, 'down': 5, 'left': 6, 'right': 7}


def location(session):
    """The pointer's position in display pixels."""
    position = dict(line.split('=', 1) for line in session.run('xdotool', 'getmouselocation', '--shell').split())
    return int(position['X']), int(position['Y'])


def glide_steps(start, end):
    """Where the pointer passes on its way from START to END, and how long it takes over each step: a fraction of a
    second, eased in and out, so that someone watching can follow it."""
    (x0, y0), (x1, y1) = start, end
    seconds = min(max(math.hypot(x1 - x0, y1 - y0) / 1500, 0.3), 0.8)
    steps = int(seconds * 60)
    points = []
    for i in range(1, steps + 1):
        s = i / steps
        s = s * s * (3 - 2 * s)
        points.append((int(x0 + (x1 - x0) * s + 0.5), int(y0 + (y1 - y0) * s + 0.5)))
    return points, seconds / steps


def to(session, x, y, glide=False):
    """Moves the pointer to display pixels X Y: at once, or in a glide that someone watching can follow if GLIDE is
    set or a recording is running."""
    if not glide and not record.running(session):
        session.run('xdotool', 'mousemove', '--', str(x), str(y))
        return
    points, pause = glide_steps(location(session), (x, y))
    # One xdotool call that alternates moves and sleeps.
    command = ['xdotool']
    for px, py in points:
        command += ['mousemove', '--', str(px), str(py), 'sleep', f'{pause:.4f}']
    session.run(*command)


def move_to(session, x, y, glide=False):
    """Moves the pointer to X Y in the pixels of the last observation."""
    addressed = session.addressed()
    x, y = integer(x, 'x'), integer(y, 'y')
    to(session, scale(x, addressed.sent_width, addressed.width), scale(y, addressed.sent_height, addressed.height),
       glide)


def click(session, button, args):
    repeat = 1
    if button == 'double':
        button, repeat = 1, 2
    elif button == 'triple':
        button, repeat = 1, 3
    # Without coordinates the click lands where an earlier move put the pointer, on what was there.
    if len(args) == 2:
        move_to(session, *args)
    elif not args:
        session.addressed()
    else:
        raise DesktopError('click takes X and Y, or nothing')
    session.run('xdotool', 'click', '--repeat', str(repeat), '--delay', '60', str(button))
    session.sync()


def move(session, args):
    if len(args) != 2:
        raise DesktopError('move takes X and Y')
    move_to(session, *args)
    session.sync()


def button(session, action, args):
    pressed = args[0] if args else '1'
    integer(pressed, 'button')
    # A release is never refused, so that a resize cannot leave a button held.
    if action != 'mouseup':
        session.addressed()
    session.run('xdotool', action, pressed)
    session.sync()


def drag(session, args):
    if len(args) != 4:
        raise DesktopError('drag takes X1 Y1 X2 Y2')
    # Checked before pressing, so that a bad drop point cannot leave the button held.
    for value, name in zip(args, ('x', 'y', 'x', 'y')):
        integer(value, name)
    move_to(session, args[0], args[1])
    # Whatever happens from the press on, even a signal while xdotool presses, the button is released, so that a failed
    # or interrupted drag cannot leave it held.
    try:
        session.run('xdotool', 'mousedown', '1')
        # A glide, since applications that track drags, such as HTML drag and drop, ignore a jump. It glides whether or
        # not a recording is running, so that a drag behaves the same in a recording as in the warm-up before it.
        move_to(session, args[2], args[3], glide=True)
    finally:
        session.run('xdotool', 'mouseup', '1', check=False)
    session.sync()


def scroll(session, args):
    if not args:
        raise DesktopError('scroll takes a direction')
    steps = integer(args[1] if len(args) > 1 else '3', 'scroll steps')
    if args[0] not in BUTTONS:
        raise DesktopError('scroll direction must be up, down, left or right')
    # It scrolls what is under the pointer, which a resize may have moved.
    session.addressed()
    session.run('xdotool', 'click', '--repeat', str(steps), '--delay', '40', str(BUTTONS[args[0]]))
    session.sync()


def cursor(session, args):
    x, y = location(session)
    addressed = session.addressed()
    x, y = scale(x, addressed.width, addressed.sent_width), scale(y, addressed.height, addressed.sent_height)
    print(json.dumps({'x': x, 'y': y}, separators=(',', ':')) if session.json else f'{x} {y}')
