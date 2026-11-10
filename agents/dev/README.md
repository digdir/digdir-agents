# Agent platform development agent

This agent develops the agent platform itself: `agentctl/`, `sandbox/` and the Rust workspace in digdir-agents. Each
agent clones `digdir/digdir-agents`, `digdir/microsandbox` and `digdir/libkrunfw` at boot.

| Agent | Image |
| --- | --- |
| `minimal` | `agentctl-dev-minimal`, published |
| `desktop` | `agentctl-dev-desktop`, published: `minimal` plus Chromium and a graphical desktop |

Both agents are named `agentctl-dev`, since you typically use one or the other. Pass `--name` to run both at once. Each
has these variants:

| Variant | What it changes |
| --- | --- |
| `nested` | Sized to fit inside another agent, as `agentctl-dev-nested` |
| `nested-build` | `nested`, with the image built from this checkout, to try a change to it before CI publishes it |
| `worktree` | Mounts this checkout in place of a fresh clone, as `agentctl-dev-worktree`, and has the agent work on it as it is |

Nested agents and the end-to-end tests need nested sandboxes, which a Linux x86_64 host with nested KVM or an Apple M3
or newer on macOS 15 or newer can provide. The manifests ask for them (`nestedSandboxes: preferred`), and on other
hosts the agent starts without them.

The images are built from this directory's `Dockerfile` on the
[agentctl base images](../../images/agentctl-base/README.md) and published to `ghcr.io/digdir/digdir-agents` by
[dev-images.yml](../../.github/workflows/dev-images.yml) whenever they or the base images change on `main`. See
[Changing them](../../images/agentctl-base/README.md#changing-them) for changes to the base images.

## Getting started

```sh
make user-install
agentctl claude login
agentctl github login default
agentctl secret set aivar --hosts api.aivar.no   # optional, for Pi
cd agents/dev/minimal
agentctl apply --wait
agentctl attach session/s1
```

Use `agents/dev/desktop` instead for a browser and a desktop. `agentctl vnc --web` prints an address for viewing
that desktop in your browser. `agentctl tui`, run from anywhere in this checkout, offers both agents and their variants
when it creates an agent.

To try a change to the image, apply the `nested-build` variant, which builds it from this checkout:

```sh
agentctl apply --variant nested-build --wait
agentctl create session/s1 --variant nested-build --harness codex
```

A variant of your own, such as `agent.mine.yaml`, which Git ignores, can extend any manifest beside it. Keep
credentials out of it: they belong in `agentctl github login` and `agentctl secret set`.

## Credentials

`agentctl github login default` signs in as a GitHub user made for the agent, or takes a fine-grained token with
`Contents` and `Pull requests` read and write. The agent pushes and opens pull requests as that account, and Git commits
as it.

The `aivar` secret is optional. It is the key for the company AI gateway at `api.aivar.no`, which Pi uses. Without it
Pi starts but rejects every prompt, so `agentctl prompt --wait` times out.

## Pi

Pi's provider and models come from [`home/.pi/agent/models.json`](home/.pi/agent/models.json), in the shared home
overlay. `agentd` writes the overlay into the sandbox every time it reconciles the agent, so edit it here rather than
in the sandbox.

Before adding a model, ask the gateway what it offers. Its model list has only the IDs:

```sh
curl -fsS -H "Authorization: Bearer $AIVAR_API_KEY" https://api.aivar.no/v1/models
```

To find the context size, send a chat completion to `/v1/chat/completions` with an oversized `max_tokens`. The error
names the model's total token limit. Add an image to the request to see whether the model reads images. A model that
reads only text rejects it.

Then add the model and try it in Pi with a prompt that runs a shell command, at each thinking level. The gateway's
answers do not show whether a model calls tools, accepts Pi's requests or supports every thinking level. Map the
levels it rejects onto ones it accepts with `thinkingLevelMap`, and leave out a model that cannot call tools.

## Inside the agent

[`instructions.md`](instructions.md), its stage's `environment.md` and [`improving.md`](improving.md) are the agent's
own instructions, with [`worktree.md`](worktree.md) on the `worktree` variant. Its skills are [`pr-evidence`](skills/pr-evidence/SKILL.md), for showing a change to reviewers, and
[`changelog`](skills/changelog/SKILL.md), for changelog entries. The images add the base image's `recording` skill,
and on `desktop` its `computer-use` skill for the desktop.
