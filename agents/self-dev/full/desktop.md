## The browser and the desktop

This Agent has a graphical desktop and Chromium. Drive them with the `computer-use` skill, and record browser captures
for pull requests as the `pr-evidence` skill describes.

Chromium trusts the mediated CA a few seconds after boot, once `$XDG_RUNTIME_DIR/chromium-ca-ready` exists. If a
browser reports a certificate error early in a Session, wait for that file and reopen the browser.

To test the platform's VNC access end to end, use a nested `full` Agent. Only its image offers VNC.
