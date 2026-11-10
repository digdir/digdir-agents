---
name: pr-evidence
description: Show pull request reviewers a change to the agentctl and agentd developer experience with recordings and screenshots, of a terminal and, on the desktop agent, of a page in a browser or the desktop. Use when a change can be seen, such as in CLI output, the TUI, a page in a browser or the desktop.
---

# Show the change to reviewers

A reviewer knows only the pull request and its issue, and should understand the experience without running it. Record
the scenario, the change and its result as the `recording` skill describes, which covers making a recording a viewer
can follow and checking it, for a terminal and for what this agent can show besides. When a before and after helps,
record both, as it describes for a comparison.

A still picture is enough when the change does not move, and quicker to review: the text of CLI output in a code block,
or on the `desktop` agent a page with `playwright-cli screenshot --filename=page.png`.

- Keep artifacts under `/home/agent/code/.artifacts/<task>/<run>/`, outside the checkout.
- Record the tested revision, with test data and no secrets.
- Keep each file within 10 MiB, and each clip short enough to follow without seeking. Split a longer demonstration.
- Give each recording a caption in the body that says what it starts from and what it shows, with what the `recording`
  skill says belongs beside a recording, such as the keys that matter.

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
