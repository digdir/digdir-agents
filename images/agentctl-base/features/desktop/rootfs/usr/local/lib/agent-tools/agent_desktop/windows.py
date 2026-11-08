"""Windows, the display and the session: listing and focusing windows, resizing the display, describing it."""

import json
import re
import time

from .geometry import scale
from .session import DesktopError, integer


def client_list(output):
    """Window ids from the EWMH property the window manager maintains. wmctrl reads the same property but segfaults on
    an empty list, and xdotool and xprop are needed here regardless."""
    _, _, ids = output.partition('#')
    return [window.strip() for window in ids.split(',') if window.strip()]


def window_class(output):
    """The class of WM_CLASS, its second string."""
    strings = re.findall(r'"((?:[^"\\]|\\.)*)"', output)
    return strings[1] if len(strings) > 1 else (strings[0] if strings else '')


def window_ids(session):
    return client_list(session.run('xprop', '-root', '_NET_CLIENT_LIST', check=False))


def title(session, window):
    return session.run('xdotool', 'getwindowname', window, check=False).rstrip('\n')


def windows(session, args):
    active = session.run('xdotool', 'getactivewindow', check=False).strip() or '0'
    addressed = session.addressed()
    listed = []
    for window in window_ids(session):
        # A window can close between listing and inspecting it. It is then simply not listed.
        shell = session.run('xdotool', 'getwindowgeometry', '--shell', window, check=False)
        if not shell:
            continue
        geometry = dict(line.split('=', 1) for line in shell.split())
        listed.append({
            'id': window,
            'active': geometry['WINDOW'] == active,
            'x': scale(int(geometry['X']), addressed.width, addressed.sent_width),
            'y': scale(int(geometry['Y']), addressed.height, addressed.sent_height),
            'width': scale(int(geometry['WIDTH']), addressed.width, addressed.sent_width),
            'height': scale(int(geometry['HEIGHT']), addressed.height, addressed.sent_height),
            'class': window_class(session.run('xprop', '-id', window, 'WM_CLASS', check=False)),
            'title': title(session, window),
        })
    if session.json:
        print('[')
        print(',\n'.join('  ' + json.dumps(entry, separators=(',', ':'), ensure_ascii=False) for entry in listed))
        print(']')
        return
    for entry in listed:
        print(f"{entry['id']} {entry['class'][:22]:<22} {entry['x']:>4},{entry['y']:<4} "
              f"{entry['width']:>4}x{entry['height']:<4} {'* ' if entry['active'] else '  '}{entry['title']}")


def focus(session, args):
    if not args:
        raise DesktopError('focus takes a window id or a title substring')
    target = ' '.join(args)
    match = target if target.startswith('0x') else next(
        (window for window in window_ids(session) if target.lower() in title(session, window).lower()), '')
    if not match:
        raise DesktopError(f"no window matches '{target}'")
    # Never --sync here: xdotool blocks indefinitely when the window manager declines the request.
    session.run('xdotool', 'windowactivate', match)
    session.sync()
    print(f'focused {match} {title(session, match)}')


def resize(session, args):
    if len(args) != 2:
        raise DesktopError('resize takes WIDTH and HEIGHT')
    width, height = integer(args[0], 'width'), integer(args[1], 'height')
    screens = session.run('xrandr')
    output = next((line.split()[0] for line in screens.splitlines() if ' connected' in line), '')
    mode = f'{width}x{height}'
    if not session.succeeds('xrandr', '--output', output, '--mode', mode):
        modes = ' '.join(re.findall(r'^ +(\d+x\d+)', screens, re.MULTILINE))
        raise DesktopError(f'resize: {mode} is not a mode of {output}; available: {modes} ')
    # A deliberate resize is not a surprise: coordinates follow the new geometry until the next observation.
    session.forget_observed()
    display(session, [])


def display(session, args):
    observed = session.geometry()
    try:
        query = session.run('setxkbmap', '-query')
    except DesktopError:
        raise DesktopError(f'cannot read the keyboard layout of display {session.display}') from None
    keyboard = {name: re.search(rf'^{name}: *(.*)$', query, re.MULTILINE) for name in ('layout', 'variant', 'options')}
    keyboard = {name: match.group(1) if match else '' for name, match in keyboard.items()}
    budget = session.budget
    ratio = f'{observed.sent_width / observed.width:.2f}'
    source = 'override' if budget.overrides else budget.source
    if session.json:
        # The scale keeps its two decimals, as a number.
        print(json.dumps({
            'display': session.display, 'width': observed.width, 'height': observed.height,
            'sentWidth': observed.sent_width, 'sentHeight': observed.sent_height, 'scale': 'SCALE',
            'budget': {'pixels': budget.pixels, 'maxEdge': budget.edge, 'maxWidth': budget.width,
                       'maxHeight': budget.height, 'source': source, 'harness': budget.harness or None,
                       'overrides': list(budget.overrides)},
            'keyboard': keyboard,
        }, separators=(',', ':'), ensure_ascii=False).replace('"scale":"SCALE"', f'"scale":{ratio}', 1))
        return
    print(f'display {session.display} is {observed.width}x{observed.height}; screenshots are sent at '
          f'{observed.sent_width}x{observed.sent_height} ({ratio}x)')
    limits = []
    if budget.pixels:
        limits.append(f'{budget.pixels} pixels')
    if budget.edge:
        limits.append(f'a {budget.edge} px longest edge')
    if budget.width:
        limits.append(f'a {budget.width}x{budget.height} box')
    origin = {'harness': f'for harness {budget.harness}',
              'unknown': f"the default, for unknown harness '{budget.harness}'"}.get(
                  budget.source, 'the default, without AGENT_HARNESS')
    if budget.width:
        origin = f"set by {' '.join(budget.overrides)}"
    elif budget.overrides:
        origin = f"set by {' '.join(budget.overrides)}, otherwise {origin}"
    print(f"screenshot budget {', '.join(limits)}: {origin}")
    variant = f", variant {keyboard['variant']}" if keyboard['variant'] else ''
    options = f", options {keyboard['options']}" if keyboard['options'] else ''
    print(f"keyboard layout {keyboard['layout']}{variant}{options}")


def wait(session, args):
    if len(args) != 1:
        raise DesktopError('wait takes a number of seconds')
    if not re.fullmatch(r'[0-9.]+', args[0]):
        raise DesktopError(f"wait takes a number of seconds, got '{args[0]}'")
    try:
        time.sleep(float(args[0]))
    except ValueError:
        raise DesktopError(f"wait takes a number of seconds, got '{args[0]}'") from None
