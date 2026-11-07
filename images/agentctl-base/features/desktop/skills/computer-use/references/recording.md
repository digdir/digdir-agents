# Recording for someone to watch

Someone watching a recording knows only what came with it, such as an issue, a pull request or a message. They have to
be able to tell where it starts, what each action does and what the result means, without having run it themselves.

## Make it possible to follow

- **Start from a screen they recognize**, such as an application's start page or a shell prompt, and do the setup
  before recording.
- **Reach what you show the way its user would.** For a bug, follow the steps that lead to it. For a feature, start
  where someone would find it. Creating the state behind the scenes, such as writing it to a database or a file, shows
  the outcome without showing it can happen. When there is no other way, say how the state arose alongside the
  recording.
- **Make every change on screen follow an action they can see.** Navigating by URL or reloading during the recording
  looks like the page changed on its own. A key press leaves no trace on screen, so click into a field rather than
  tabbing to it, and name the keys that matter in the caption.
- **Show a comparison with the same steps and input on both sides.** Write the steps as a script and run it for each.
  Save it as a file and run it with `bash`: a tool that wraps the commands it is given, such as in `… && pwd`, can turn
  off the `set -e` the script relies on to stop at a failed step.
- **Warm up first.** Go through the flow once before recording, since the first load of a page or an application is
  slower than the next. Wait for the element the next step needs, or for what only the result shows, rather than
  sleeping a fixed time.
- **Hold each result long enough to read**, and keep the pointer beside it rather than on it, where a tooltip can cover
  it.

## Check it as the viewer would

A command that succeeded has not shown that the recording does. Before handing it over, run `media-preview FILE`. It
prints when the picture stays still long enough to read, which is all the viewer gets to read, and writes a contact
sheet with the last frame of each of those times, or evenly spaced frames for a clip that is rarely still. Read the
sheet as the viewer's story: every step and result should be there, in order, and not covered by the pointer. Where
what decides it is too small to read, look at that frame at full size with
`ffmpeg -ss SECONDS -i FILE -frames:v 1 frame.png`.

## A web page

A Playwright recording of the page alone suits a flow that stays on one page or one site, and needs no coordinates.
The `playwright-cli` skill's video reference covers chapter cards, labelled actions and attaching the video, which
GitHub plays as it is. Size the video to the viewport, since it is otherwise scaled down to fit 800 by 800 pixels, and
let `--cursor` draw a pointer that travels to each action:

```sh
export PLAYWRIGHT_CLI_SESSION=<task>           # a browser of this task's own
playwright-cli open --browser chromium <start-url>
playwright-cli resize 1280 720
playwright-cli video-start take.webm --size 1280x720 --cursor
playwright-cli run-code --filename=scenario.js; status=$?
playwright-cli video-stop
[ "$status" = 0 ] && mv take.webm page.webm    # keep the last good take
```

Open the starting page before `video-start`, and only click or type after it. `scenario.js` holds an async function
taking Playwright's `page`, so add chapter cards and labels inside it, with `page.screencast.showChapter` and
`page.screencast.showActions`. Give its waits a short timeout, such as with `page.setDefaultTimeout(15000)`, so that a
failure does not leave a long stretch of dead video. A take that was interrupted leaves the browser recording, so run
`playwright-cli video-stop` before the next.

## The desktop

`desktop record` captures the whole display. It suits a flow across windows, through the browser's own controls or in
a native application. While it records, the pointer glides to where `desktop` commands send it and `desktop type`
types at a readable pace. Act through `desktop` for that reason: `playwright-cli click` and `fill` work on the page
without moving the desktop's pointer at all, so keep `playwright-cli` to waiting for the page.

Go through the flow once and return to the start. For a page, open it in a headed browser with
`playwright-cli open --browser chromium --headed`, and click an empty part of it so that the address bar is not
selected. Take a screenshot with `desktop screenshot` to measure the controls, then write the steps to `take.sh` and
run `bash take.sh`. For a page it looks like this:

```sh
set -e                                     # a failed step ends the take
# Only a take that reached its last line replaces result.mp4, not one that failed or was interrupted.
trap 'desktop record stop; [ -n "${finished:-}" ] && mv take.mp4 result.mp4' EXIT
wait_for() {                               # says why, when it fails
  local out
  out=$(playwright-cli run-code "async page => page.$1.waitFor({ timeout: 15000 })") || { echo "$out" >&2; return 1; }
}

desktop record start take.mp4
sleep 2                                    # the starting screen, to orient the viewer
desktop click 500 453
desktop type '10,48'                       # the shell reads it first, so quote it
desktop click 640 453
wait_for "getByText('Total: 10,48')"       # what only the result shows
desktop move 700 597                       # beside the result
sleep 4
finished=1
```

Coordinates refer to the screenshot taken before the script. A failed take is left in `take.mp4` for finding out what
went wrong, and is not the recording to hand over.

`wait_for` sees only the page. In a native window, wait for what its tree shows, with a time limit, such as
`timeout 15 sh -c 'until desktop tree sakura | grep -q Done; do sleep 0.5; done'`. A terminal's tree holds only its last
few lines, so check anything above them with a screenshot.
