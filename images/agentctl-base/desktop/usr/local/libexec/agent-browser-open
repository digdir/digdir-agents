#!/bin/sh
# Opens links for xdg-open. On a desktop it does not recognize, such as this openbox one, xdg-open runs the handler
# in the foreground and waits for it to exit, where GNOME's and KDE's openers start the browser in the background.
# This does the same, so commands such as `gh pr view --web` return once the browser has started, not when it closes.
exec setsid -f chromium "$@" </dev/null >/dev/null 2>&1
