---
name: pr-evidence
description: Help pull request reviewers understand changes to the agentctl and agentd developer experience through terminal recordings and, on the full agent, browser captures. Use when a change can be seen, such as in CLI output, the TUI or a page in a browser.
---

# Show the change to reviewers

Demonstrate the scenario, the relevant change and its result so a reviewer can understand the experience without
running it locally. Explain the premise and starting state in the recording or its caption in the pull request.

- Keep artifacts under `/home/agent/code/.artifacts/<task>/<run>/`, outside the checkout.
- Keep each attachment within 10 MiB, and each GIF short enough to follow without seeking. Split longer
  demonstrations into clips.
- Capture actual behavior from the tested revision, using test data without secrets.

On the `full` agent you can also capture a browser, as [browser.md](references/browser.md) describes.

## Record

Do the setup before recording, so the demonstration shows what a user would type: plain command names on `PATH` from
a sensible working directory, not full binary paths, custom environment variables or a custom `HOME`. When such
configuration is the point of the change, show it and say why.

Give the recorder a colour-capable terminal, which the program inherits. The `env` prefix below removes `NO_COLOR` and
replaces an inherited `TERM=dumb`. Leave it out when the change is about behavior under those settings. From the
artifact directory:

```sh
env -u NO_COLOR TERM=xterm-256color COLORTERM=truecolor \
  asciinema rec --window-size 120x36 --command 'agentctl tui' terminal.cast
agg --font-size 14 terminal.cast terminal.gif
agg --select 50% terminal.cast frame.gif
```

For a scripted CLI demonstration, replace `agentctl tui` with `bash demo.sh` and have the script display the commands
it runs. Pause before execution and after output so a human can follow along. For a TUI, pause on relevant states
before moving on. `--idle-time-limit` can compress long waits, but preserve enough time to read.

For a program in a container, pass the capabilities through with `podman run -e TERM -e COLORTERM ...` and make sure
`NO_COLOR` is unset inside it. If picker glyphs are missing, try `agg --font-family 'JetBrains Mono'`. Before
attaching, look at frames from several positions with `agg --select`, not only the first, to check they are readable.

## Attach

Write the pull request body with local image references, and run `gh` from the artifact directory with one `--attach`
per file. It uploads the files and rewrites the references to hosted URLs. After pushing the branch:

```sh
gh pr create --repo digdir/digdir-agents --base main --head <branch> \
  --title 'fix: ...' --body-file pr-body.md --attach ./result.gif
gh pr edit <number> --attach ./result.gif
```

If an upload fails, look at the pull request before retrying: a partial success can create or update it despite a
nonzero exit. Retry missing attachments with `gh pr edit` rather than creating it again.

Read the body back with `gh pr view <number> --json body -q .body` and confirm attachments have hosted URLs.
