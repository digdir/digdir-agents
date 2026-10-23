# digdir-agents

Digdir's Agent platform: `agentctl` runs long-running Agents in isolated Sandboxes, built on a reusable Sandbox SDK.
Agents get their own computer, with mediated network access and no real secrets inside the Sandbox.

> This README is a short placeholder while the repository is being cleaned up. A full rewrite with an architecture
> diagram follows.

## Repository layout

| Path | What it is |
| --- | --- |
| [`agentctl/`](agentctl/) | The `agentctl` CLI and the `agentd` Session API backend, independent of model and harness. Start here to install and run an Agent. |
| [`sandbox/`](sandbox/) | The Sandbox SDK, with an implementation on Digdir's forks of [microsandbox](https://github.com/digdir/microsandbox) and [libkrunfw](https://github.com/digdir/libkrunfw). |
| [`images/agentctl-base/`](images/agentctl-base/) | Base images that Agent images build on, headless and desktop, with Claude Code, Codex and Pi. |
| [`agents/self-dev/`](agents/self-dev/) | The Agent that develops this platform. It is also the example to start from when you make a new Agent. |
| [`integrations/`](integrations/) | Slack and GitHub listener from the earlier orchestrator pipeline. It is kept as a possible entry point to agentctl Agents, but nothing consumes its events right now. |
| [`docs/presentations/`](docs/presentations/) | Slide decks on agentctl and the Sandbox SDK, including a security view. |

## Getting started

Install `agentctl` and run an Agent as described in [`agentctl/README.md`](agentctl/README.md). To work on the
platform itself, use the self-dev Agent: see [`agents/self-dev/README.md`](agents/self-dev/README.md).

## History

Until October 2026 this repository also held an orchestrator pipeline: a set of Agents fed through a file contract
from `integrations/`, an LLM gateway, host scripts and a knowledge base under `doc/`. It is archived as the tag
[`v2.0-pre-cleanup`](https://github.com/digdir/digdir-agents/tree/v2.0-pre-cleanup).
