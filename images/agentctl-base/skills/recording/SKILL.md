---
name: recording
description: Record a terminal, a web page or the desktop so that someone who was not there can follow it, and check the recording as they will see it. Use when recording a flow or a demonstration for someone else, such as a reviewer.
---

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
- **Show a comparison with the same steps and input on both sides**, by running the same take script for each.
- **Warm up first.** Go through the flow once before recording, since the first load of a page or an application is
  slower than the next. Wait for the element the next step needs, or for what only the result shows, rather than
  sleeping a fixed time.
- **Hold each result long enough to read**, and keep the pointer beside it rather than on it, where a tooltip can cover
  it.

## Record a take with a script, and keep the last good one

Write the steps of a take to a script, so that a retake or the other side of a comparison does the same. Save it as a
file and run it with `bash take.sh`: a tool that wraps the commands it is given, such as in `… && pwd`, can turn off
the `set -e` the script relies on to stop at a failed step.

Record each take to its own file, such as `take.mp4`, and let only a take that finished replace the one you hand over,
such as `result.mp4`. A take that failed or was interrupted then never replaces a good one, and is still there for
finding out what went wrong. Each reference below shows how for its medium.

## Check it as the viewer would

A command that succeeded has not shown that the recording does. Before handing it over, run `media check FILE`, which
the browser feature brings and `desktop record stop` runs for you. It prints when the picture holds still long enough to
read, which is all the viewer gets to read, and writes a contact sheet, `FILE.preview.png`, of the last frame of the
last of those times, which shows the result, and of the five longest others. A clip that is rarely still gets evenly
spaced frames up to the result instead. The sheet must show the result, not covered by the pointer, but it has no room
for every step. Check the other times `media check` lists, and anything too small to read, with
`media frames FILE SECONDS...`, which writes those frames at full size.

It compares brightness, so a change of colour alone, such as a badge turning from red to green, counts as still. Look
at such a change yourself.

## Each medium

Read the reference for what you record. A feature of the image installs each one beside this file, so a reference that
is missing is for something this image cannot record.

- [references/terminal.md](references/terminal.md): a terminal, with `asciinema` and `agg`. Every image has it.
- [references/web.md](references/web.md): a web page in Chromium, with `playwright-cli`. The browser feature installs
  it.
- [references/desktop.md](references/desktop.md): the whole desktop, with `desktop record`, for a flow across windows,
  through the browser's own controls or in a native application. The desktop feature installs it.
