# Recording a web page

A Playwright recording of the page alone suits a flow that stays on one page or one site, and needs no coordinates.
The `playwright-cli` skill's video reference covers chapter cards, labelled actions and attaching the video, which
GitHub plays as it is. What follows adds to it.

Size the video to the viewport, since it is otherwise scaled down to fit 800 by 800 pixels, and let `--cursor` draw a
pointer that glides to each click. It stays where it was for `fill`, `selectOption` and typing, so click a field first
when the viewer should see where its value goes:

```sh
export PLAYWRIGHT_CLI_SESSION=<task>           # a browser of this task's own
playwright-cli open --browser chromium <start-url>
playwright-cli resize 1280 720
playwright-cli video-start take.webm --size 1280x720 --cursor
playwright-cli run-code --filename=scenario.js; status=$?
playwright-cli video-stop
[ "$status" = 0 ] && mv take.webm page.webm    # keep the last good take
media check page.webm
```

Open the starting page before `video-start`, and only click or type after it. `scenario.js` is the take's script: an
async function taking Playwright's `page`, so add chapter cards and labels inside it, with `page.screencast.showChapter`
and `page.screencast.showActions`. Give its waits a short timeout, such as with `page.setDefaultTimeout(15000)`, so
that a failure does not leave a long stretch of dead video.

Standalone Playwright scripts work too. Run them with `NODE_PATH="$(npm root -g)" node script.js`, and in an ES module
load Playwright with `createRequire(import.meta.url)('playwright')`, since `import` ignores `NODE_PATH`.

A take that was interrupted leaves the browser recording, so run `playwright-cli video-stop` before the next.

A select's list and a date picker are drawn outside the page, so the recording shows the value change without them.
When choosing is the point, record the desktop instead, as [desktop.md](desktop.md) describes, on an image with the
desktop feature.
