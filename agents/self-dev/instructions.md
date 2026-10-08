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
`make user-install` work here too: the Sandbox has `/dev/kvm` and Podman.

Do not add `Co-Authored-By` or similar AI-attribution trailers to commit messages or pull request descriptions.

To run a nested Agent, log the nested `agentd` in with the placeholders this Sandbox already holds, then apply the
`nested` variant from the task's worktree: it builds its image from the checkout it is applied from, so applying it
from the primary checkout tests `main` instead of your change. Use `agents/self-dev/full` instead of `minimal` for a
nested Agent with a browser and a desktop. An Agent's source directory cannot change, so delete an earlier
`agentctl-dev-nested` applied from another worktree first.

```sh
printf '%s\n' "$AGENT_CLAUDE_ACCESS_TOKEN" | agentctl claude login --from-stdin
agentctl codex login --from-stdin < ~/.codex/auth.json
printf 'GITHUB_TOKEN=%s\nGIT_USER_NAME=%s\nGIT_USER_EMAIL=%s\n' \
  "$GITHUB_TOKEN" "$GIT_USER_NAME" "$GIT_USER_EMAIL" > ~/nested.env
cd /home/agent/code/.worktrees/<task>/agents/self-dev/minimal
agentctl apply --variant nested --env-file ~/nested.env
```

Real secrets are host-mediated: never search for, print, copy or persist their values. The credential placeholder
above is inert. Git identity is explicitly selected non-secret data and enters both Sandboxes in plaintext.

Build steps inside Podman trust the mediated CA through the system store and `/run/agent/tls/ca-bundle.pem`. Buildah
drops default environment from build stages, so a `RUN` that downloads through Node exports
`NODE_EXTRA_CA_CERTS=/run/agent/tls/ca-bundle.pem` when that file is readable. Do not persist that with Dockerfile
`ENV`.
