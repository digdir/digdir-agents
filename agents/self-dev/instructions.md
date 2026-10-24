# Agent platform self-development Agent

You develop the agent platform in the checkout at `/home/agent/code/digdir-agents`: `agentctl/`, `sandbox/` and the
Rust workspace at its root. Never delete, reset or reclone that directory. If the checkout is absent, run
`gh repo clone digdir/digdir-agents /home/agent/code/digdir-agents`. The Microsandbox and libkrunfw forks it builds on
are cloned beside it, at `/home/agent/code/microsandbox` and `/home/agent/code/libkrunfw`, with `upstream` remotes for
their upstream repositories; each fork's `CONTRIBUTING-digdir.md` describes how to change it.

Keep the primary checkout clean for synchronizing remotes and managing worktrees. Do each task in its own Git worktree
under `/home/agent/code/.worktrees/`, starting new work from the current `origin/main`. Run the task's `make` commands
and the `pr-evidence` workflow from that worktree; `make user-install` installs the build from the worktree where it
runs.

Read `agentctl/AGENTS.md` first. Pull requests that change `agentctl` output or the TUI include a terminal
recording; the `pr-evidence` skill describes how to record and attach it. The `changelog` skill describes how to write
`agentctl/CHANGELOG.md` entries. `make help` at the worktree's root
lists the targets; run `make fmt lint build test` before reporting completion. `make test-e2e` and
`make user-install` work here too: the Sandbox has Podman, and `/dev/kvm` unless the host cannot provide it.

The `agentctl` skill describes running nested Agents. Here, install `agentctl` with `make user-install` from the task's
worktree, add `AIVAR_API_KEY`, which Pi uses, to the nested environment file, and apply the `nested` variant from the
same worktree: it builds its image from the checkout it is applied from, so applying it from the primary checkout tests
`main` instead of your change.

```sh
cd /home/agent/code/.worktrees/<task>/agents/self-dev/minimal
agentctl apply --variant nested --env-file ~/nested.env --wait
```

Its build pulls the published agentctl base image, replacing a local image under the same name. To test a change to
`images/agentctl-base`, build it from the worktree under a `localhost/` tag, as its README describes, and point
`agents/self-dev/Dockerfile`'s `FROM` at it while you test. Use `agents/self-dev/full` instead of `minimal` for a nested
Agent with a browser and a desktop. Delete an earlier `agentctl-dev-nested` applied from another worktree first, and
delete it before removing the worktree it reads from.

Build steps inside Podman trust the mediated CA through the system store and `/run/agent/tls/ca-bundle.pem`. Buildah
drops default environment from build stages, so a `RUN` that downloads through Node exports
`NODE_EXTRA_CA_CERTS=/run/agent/tls/ca-bundle.pem` when that file is readable. Do not persist that with Dockerfile
`ENV`.
