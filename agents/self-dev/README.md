# Agent platform self-development Agent

This Agent develops the Agent platform itself: `agentctl/`, `sandbox/` and the Rust workspace in digdir-agents. Each
Agent clones `digdir/digdir-agents`, `digdir/microsandbox` and `digdir/libkrunfw` at boot.

| Agent | Image |
| --- | --- |
| `minimal` | `agentctl-self-dev-minimal`, published |
| `full` | `agentctl-self-dev-full`, published: `minimal` plus Chromium and a graphical desktop |

Each has a `nested` variant that builds the same image from this checkout and is sized to fit inside another Agent.
Both Agents are named `agentctl-dev`, and their nested variants `agentctl-dev-nested`, since you typically use one or
the other. Pass `--name` to run both at once.

Nested Agents and the end-to-end tests need nested Sandboxes, which a Linux x86_64 host with nested KVM or an Apple M3
or newer on macOS 15 or newer can provide. The manifests ask for them (`nestedSandboxes: preferred`), and on other
hosts the Agent starts without them.

The images are built from this directory's `Dockerfile` on the [agentctl base images](../../images/agentctl-base/README.md)
and published to `ghcr.io/digdir/digdir-agents` whenever they or the base images change on `main`. See
[Changing them](../../images/agentctl-base/README.md#changing-them) for changes to the base images.

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

To try a change to the image, apply the `nested` variant, which builds it from this checkout:

```sh
agentctl apply --variant nested --env-file ~/.agent/self-dev.env --wait
agentctl create session/s1 --variant nested --harness codex
```

A variant of your own, such as `agent.mine.yaml`, which Git ignores, can extend any manifest beside it. Keep
credentials outside the checkout.

## Credentials

`GITHUB_TOKEN` is a [fine-grained personal access token](https://github.com/settings/personal-access-tokens/new) with
`Contents` and `Pull requests` read and write.

`AIVAR_API_KEY` is optional. It is the key for the company AI gateway at `api.aivar.no`, which Pi uses. Without it Pi
starts but rejects every prompt, so `agentctl prompt --wait` times out.

## Pi

Pi's provider and models come from [`home/.pi/agent/models.json`](home/.pi/agent/models.json), in the shared home
overlay. `agentd` writes the overlay into the Sandbox every time it reconciles the Agent, so edit it here rather than
in the Sandbox.

## Inside the Agent

[`instructions.md`](instructions.md), plus [`full/desktop.md`](full/desktop.md) on `full`, are the Agent's own
instructions. Its skills are [`pr-evidence`](skills/pr-evidence/SKILL.md), for showing a change to reviewers, and
[`changelog`](skills/changelog/SKILL.md), for changelog entries. The `full` image also ships the base image's
`computer-use` skill for its desktop.
