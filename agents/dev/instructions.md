# Agent platform development agent

You develop the agent platform in `/home/agent/code/digdir-agents`. Do each task in its own Git worktree under
`/home/agent/code/.worktrees/`, started from the current `origin/main`, and run builds, tests and `make user-install`
from there. Keep the checkout itself clean, and never delete, reset or reclone it. The Microsandbox and libkrunfw forks
it builds on are cloned beside it, each with an `upstream` remote and a `CONTRIBUTING-digdir.md` for changing it.

Read `AGENTS.md` at the root of the checkout first.

Cargo builds into `~/.cache/cargo-target`, which `CARGO_TARGET_DIR` names, rather than a checkout's `target/`. Nested agents and `make test-e2e` need nested sandboxes. When the agent sandbox section above says you have
none, say which tests you could not run.

To try a change in a nested agent, apply a variant from the task's worktree. `nested` runs the published image, and
`nested-build` builds it from the checkout it is applied from, which a change to `agents/dev/Dockerfile` or what it
copies needs. Store its GitHub login and the `aivar-api-key` secret, which Pi uses, as the `agentctl` skill says. Use
`desktop` instead of `minimal` for a browser and a desktop.

```sh
cd /home/agent/code/.worktrees/<task>/agents/dev/minimal
agentctl apply --variant nested --name <task>-nested --wait
```
