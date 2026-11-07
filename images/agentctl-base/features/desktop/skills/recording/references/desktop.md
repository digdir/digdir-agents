# Recording the desktop

`desktop record start OUT.mp4` captures the whole display until `desktop record stop`, which encodes it 1280 pixels
wide, or as wide as `--width` says, and prints what `media check` prints for it. It suits a native application, a flow
across windows, and the browser's own controls, such as a select's list or a date picker, which a recording of the page
alone leaves out.

While it records, the pointer glides to where `desktop` commands send it and `desktop type` types at a readable pace,
so act through `desktop` for the viewer to see each action.

## Prepare

Go through the flow once and return to the start. Windows open maximized, so leave them that way, and take a
screenshot with `desktop screenshot` to see the starting screen.

Measure the controls with `desktop tree APPLICATION`, which gives each one's box in the same pixels at a fraction of a
screenshot's cost, rather than with a screenshot of each state. While going through the flow, measure again in each
state that moves them, such as after an error message or a banner appears, so that the take needs no measuring.

## Record

Write the steps to `take.sh` and run `bash take.sh`:

```sh
set -e                                     # a failed step ends the take
wait_until() {                             # tries a check for up to 15 s, and says which one failed
  local end=$((SECONDS + 15))
  until "$@" >/dev/null 2>&1; do
    [ "$SECONDS" -lt "$end" ] || { echo "timed out waiting for: $*" >&2; return 1; }
    sleep 0.5
  done
}
shows() { desktop tree "$1" | grep -qF -- "$2"; }   # whether APPLICATION's tree shows TEXT
keep() {                                   # only a finished take replaces result.mp4 and its contact sheet
  [ -n "${finished:-}" ] && mv take.mp4 result.mp4 && mv take.mp4.preview.png result.mp4.preview.png
}

desktop record start take.mp4
# Set only once the recording is this script's, so it never stops another.
trap 'desktop record stop && keep' EXIT
sleep 2                                    # the starting screen, to orient the viewer
desktop click 500 453
desktop type '10,48'                       # the shell reads it first, so quote it
desktop click 640 453
wait_until shows <application> 'Total: 10,48'   # what only the result shows
desktop move 700 597                       # beside the result
sleep 4
finished=1
```

Coordinates refer to the screenshot or tree taken before the script. A take that failed is left in `take.mp4`, and its
check printed, for finding out what went wrong. When `record stop` itself fails or is interrupted, running
`desktop record stop` again renders the take.

## A page in the browser

Open the page in a headed browser of the task's own, as [web.md](web.md) does but with
`playwright-cli open --browser chromium --headed` and without `resize`, so that the page fills the screen the
recording shows. Click an empty part of the page so that the address bar is not selected.

`desktop tree chromium` shows the page's own headings, fields and text beside the browser's controls, so measure and
wait with it as for any window. Act through `desktop` rather than `playwright-cli click` or `fill`, which change the
page without moving the desktop's pointer.

## A terminal

A terminal's tree, such as `desktop tree sakura`, holds only its last few lines, as one value with `\n` between them.
The command you typed shows there too, so wait for text only its output has, and check anything above those lines with a
screenshot. For a recording of the terminal alone, [terminal.md](terminal.md) is lighter.
