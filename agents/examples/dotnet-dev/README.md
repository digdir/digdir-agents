# .NET development agent

An agent to start from: a computer of its own with the .NET SDK, Podman, a desktop with Chromium, Claude Code, Codex
and Pi, which clones your repositories at boot. [`agent.yaml`](agent.yaml) and the [`Dockerfile`](Dockerfile) explain
each line and hold the values we recommend. Copy this agent definition and make it your own, or have a coding agent do
it.

## Try it

Install `agentctl` and log in as its [README](../../../agentctl/README.md#install) describes, then from this
directory:

```sh
cp .env.sample .env   # then fill it in
agentctl apply --wait
agentctl tui
```

The first `apply` builds the image, which takes a few minutes. In the TUI, create a session and attach to it.
`agentctl --help` lists everything else.
