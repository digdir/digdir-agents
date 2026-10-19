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

The images are built from this directory's `Dockerfile` on the [agentctl base images](../../images/agentctl-base/README.md),
and published to `ghcr.io/digdir/digdir-agents` whenever they or the base images change on `main`. `full` is `minimal`
plus the base images' `desktop` feature. Use a `nested` variant to try changes to the image itself. It builds on the
base image Podman already has under the published name, so `podman pull ghcr.io/digdir/agentctl-base:ubuntu-26.04`
first, or build the base locally to try a change to it.

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

`AIVAR_API_KEY` is optional. It is the key for the company AI gateway at `api.aivar.no`, which Pi uses, and is likewise
substituted only in requests to that host. Without it Pi starts, but rejects every prompt and a wait for one times out.

## Pi

The `pi` installation in `agent.yaml` defaults to `aivar/sigma2:Qwen3.8-27B`, and the shared home overlay in `home/`
supplies `~/.pi/agent/models.json` with the `aivar` provider (`https://api.aivar.no/v1`, key from `$AIVAR_API_KEY`).
The overlay is applied again on every pass, so edit it here rather than in the Sandbox. An existing self-dev Agent
needs `agentctl delete` and a new `apply`: its harness types and home overlay, which moved to `home/`, are fixed
when it is created.

The findings behind `models.json`, checked through Pi 1.3.0 on 2026-10-17 with a bash tool call at every thinking
level. The limits are the gateway's own, from its error to an oversized `max_tokens` request.

| Model | Offered | Notes |
| --- | --- | --- |
| `sigma2:Qwen3.8-27B` | Yes | Accepts only the efforts `low`, `medium` and `xhigh`, so `thinkingLevelMap` maps the others onto them. |
| `azure:DeepSeek-V4.1-Flash` | Yes | Chat Completions only: the `azure:` route rejects Pi's Responses input. |
| `sigma2:Borealis2-preview` | Yes | |
| `aivar:borealis` | Yes | 32k tokens in total, and text only; the others also read images. |
| `azure:gpt-6.1-sol` | No | Rejects tool calls on both APIs. |
| `sigma2:Borealis-27B` | No | Never calls a tool. |

Re-check these when the gateway or Pi changes.

## Inside the Agent

`instructions.md` tells the harness how to build, test and run the platform nested; the `full` Agent adds
`full/desktop.md`. The `pr-evidence` skill describes how to record `agentctl` demonstrations and attach them to pull
requests, and the `changelog` skill how to write changelog entries. The `full` image also ships the `computer-use`
skill for driving its desktop.
