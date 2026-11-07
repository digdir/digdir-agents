---
name: pr-evidence
description: Show pull request reviewers a change to the agentctl and agentd developer experience with terminal recordings and, on the full agent, browser and desktop recordings. Use when a change can be seen, such as in CLI output, the TUI, a page in a browser or the desktop.
---

# Show the change to reviewers

A reviewer knows only the pull request and its issue, and should understand the experience without running it. Show
the scenario, the change and its result: start from something they recognize, reach the change the way its user would,
and when a before and after helps, record both with the same script. Say in the caption what the recording starts from.
A still picture is enough when the change does not move, and quicker to review: the text of CLI output in a code block,
or a page with `playwright-cli screenshot --filename=page.png`.

- Keep artifacts under `/home/agent/code/.artifacts/<task>/<run>/`, outside the checkout.
- Record the tested revision, with test data and no secrets.
- Keep each file within 10 MiB, and each clip short enough to follow without seeking. Split a longer demonstration.

On the `full` agent, the `computer-use` skill's `references/recording.md` describes recording a web page or the
desktop, and checking a recording as the viewer would.

## Record a terminal

Set up before recording, so that the recording shows what a user would type: commands on `PATH`, run from a sensible
directory, without full binary paths, custom environment variables or a custom `HOME`, unless that configuration is the
change. From the artifact directory:

```sh
env -u NO_COLOR TERM=xterm-256color COLORTERM=truecolor \
  asciinema rec --window-size 120x36 --command 'agentctl tui' terminal.cast
agg --font-size 14 terminal.cast terminal.gif
```

The `env` prefix gives the program colour, so leave it out only when the change is about `NO_COLOR` or `TERM`. A
program in a container needs them passed on, with `podman run -e TERM -e COLORTERM`. For a scripted demonstration,
record `bash demo.sh` and have the script print each command, pausing before it and after its output. In a TUI, pause
on each state that matters. `--idle-time-limit` shortens long waits.

Before attaching, look at frames from several points, not only the first, with `agg --select 50% terminal.cast
frame.gif`. If picker glyphs are missing, try `agg --font-family 'JetBrains Mono'`.

## Attach

Write the body with local references to the files, and run `gh` from the artifact directory with one `--attach` per
file. It uploads them and rewrites the references to hosted URLs. After pushing the branch:

```sh
gh pr create --repo digdir/digdir-agents --base main --head <branch> \
  --title 'fix: ...' --body-file pr-body.md --attach ./terminal.gif
gh pr edit <number> --attach ./terminal.gif
```

A failed upload can still have created or updated the pull request, so look at it before retrying, and retry missing
files with `gh pr edit`. Read the body back with `gh pr view <number> --json body -q .body` and check that every
attachment has a hosted URL.
