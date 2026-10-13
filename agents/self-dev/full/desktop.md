## The browser and the desktop

This computer also has `playwright-cli` with Chromium (open it with `--browser chromium`), `ffmpeg`, and the
`video-to-gif` and `media-preview` helpers, for browser captures in pull request evidence. Chromium trusts the mediated
CA once `$XDG_RUNTIME_DIR/chromium-ca-ready` exists; the import finishes in the background seconds after boot, so if a
browser reports a certificate error shortly after the Session starts, wait for that file and reopen the browser.

It has a graphical desktop on display `:1`, 1456x819, running from boot: one process that is both X server and VNC
server, the openbox window manager and a tint2 panel. `DISPLAY` is already set in every Session, and the keyboard
layout is Norwegian, so `desktop type` enters æøå and ÆØÅ.

Drive it with the `desktop` helper and the `computer-use` skill. `desktop tree` reads what is showing as an
accessibility tree, including the browser's own controls and dialogs, over the session bus at
`$DBUS_SESSION_BUS_ADDRESS`. `chromium` on `PATH` is the same Playwright browser build, so a page looks the same
whether `playwright-cli open --headed` or a person opened it.

`desktop-terminal` opens a terminal on the desktop, as `Ctrl+Alt+T` and the panel's launcher do. It loads the Session
environment SSH access writes to `~/.ssh/environment`, so commands in it behave as they do in a Session. Start it
detached (`setsid desktop-terminal -x 'COMMAND' &`) to show a command on screen; `desktop tree sakura` reads its last
lines.

A person reaches the same desktop with `agentctl vnc --web`, which prints an address to open in their browser
(`--open` opens it), or with a VNC client of their own. They share your keyboard and pointer, so say what you are about
to do before you do it, and stop when they take over.

This image provides the VNC access units agentd enables for `access: [{type: vnc}]`, so a nested `full` Agent
exercises the platform's VNC access end to end.
