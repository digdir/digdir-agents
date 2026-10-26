# Agent platform self-development agent

You develop the agent platform in `/home/agent/code/digdir-agents`. Do each task in its own Git worktree under
`/home/agent/code/.worktrees/`, started from the current `origin/main`, and run builds, tests and `make user-install`
from there. Keep the checkout itself clean, and never delete, reset or reclone it. The Microsandbox and libkrunfw forks
it builds on are cloned beside it, each with an `upstream` remote and a `CONTRIBUTING-digdir.md` for changing it.

Read `AGENTS.md` at the root of the checkout first.

To try a change in a nested agent, apply the `nested` variant from the task's worktree, since it builds its image from
the checkout it is applied from. Add `AIVAR_API_KEY`, which Pi uses, to the environment file the `agentctl` skill
has you write. Use `full` instead of `minimal` for a browser and a desktop.

```sh
cd /home/agent/code/.worktrees/<task>/agents/self-dev/minimal
agentctl apply --variant nested --name <task>-nested --env-file ~/nested.env --wait
```
