# Agent platform self-development Agent

This Agent develops the Agent platform itself: `agentctl/`, `sandbox/` and the Rust workspace in digdir-agents. Each
Agent clones `digdir/digdir-agents`, `digdir/microsandbox` and `digdir/libkrunfw` at boot.

| Agent / variant | Image | Resources |
| --- | --- | --- |
| `minimal` default | Published `agentctl-self-dev-minimal` | 4 CPUs, 8 GiB memory, 100 GiB disk |
| `minimal` `nested` | `minimal` stage built from this checkout | Reduced to fit inside another Agent |
| `full` default | Published `agentctl-self-dev-full`: `minimal` plus Chromium and a graphical desktop | 4 CPUs, 8 GiB memory, 100 GiB disk |
| `full` `nested` | `full` stage built from this checkout | Reduced to fit inside another Agent |

Both Agents are named `agentctl-dev`, and their nested variants `agentctl-dev-nested`, since you typically use one or
the other. Pass `--name` to run both at once.

The images are built from this directory's `Dockerfile` and published to `ghcr.io/digdir/digdir-agents` whenever they
change on `main`. Use a `nested` variant to try changes to the image itself.

## Getting started

```sh
make user-install
agentctl claude login
mkdir -p ~/.agent
cp agents/self-dev/minimal/.env.sample ~/.agent/self-dev.env
# Fill in ~/.agent/self-dev.env, then:
cd agents/self-dev/minimal
agentctl apply --env-file ~/.agent/self-dev.env --wait
agentctl attach session/s1
```

Use `agents/self-dev/full` instead for a browser and a desktop. `agentctl vnc --web` prints an address for viewing
that desktop in your browser. `agentctl tui`, run from anywhere in this checkout, offers both Agents and their variants
when it creates an Agent.

For a reduced Agent built from this checkout:

```sh
agentctl apply --variant nested --env-file ~/.agent/self-dev.env --wait
agentctl create session/s1 --variant nested --harness codex
```

Ignored local variants such as `agent.mine.yaml` may extend another sibling variant. Keep their credentials outside the
checkout.

## Credentials

The GitHub token stays on the host. The Agent sees a placeholder, which is substituted only in requests to GitHub.
`GIT_USER_NAME` and `GIT_USER_EMAIL` enter the Agent in plaintext.

`GITHUB_TOKEN` is a [fine-grained personal access token](https://github.com/settings/personal-access-tokens/new) with
`Contents` and `Pull requests` read and write.

## Inside the Agent

`instructions.md` tells the harness how to build, test and run the platform nested; the `full` Agent adds
`full/desktop.md`. The `pr-evidence` skill describes how to record `agentctl` demonstrations and attach them to pull
requests, and the `changelog` skill how to write changelog entries. The `full` Agent also has the `computer-use` skill
for driving its desktop.
