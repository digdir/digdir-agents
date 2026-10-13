# Minimal Agent

The smallest useful Agent: Claude Code in a Sandbox, with no repository checkout and no secrets. Start here to learn
how an Agent is put together, then look at [`agents/self-dev`](../../../agents/self-dev) for one that is used every
day.

| File | Purpose |
| --- | --- |
| [`agent.yaml`](agent.yaml) | The manifest: image, resources, instructions, harness and network |
| [`Dockerfile`](Dockerfile) | The image: Ubuntu with Git, the GitHub CLI, tmux and Claude Code |
| [`instructions.md`](instructions.md) | What Claude Code is told about this computer, installed as `~/.claude/CLAUDE.md` |
| [`claude-state.json`](claude-state.json) | Claude Code state baked into the image, so first-run prompts are skipped |
| [`home/`](home) | Files copied into `/home/agent` on every reconciliation; empty here |

## Try it

From this directory:

```sh
agentctl claude login                  # once per host
agentctl apply --wait                  # build the image and start the Agent
agentctl attach session/s1             # start Claude Code in a Session; detach with Ctrl-b d
agentctl exec -it agent/minimal -- bash
agentctl delete agent/minimal
```

`agentctl` finds the Agent from the directory you are in, so Session commands need no `--agent` here. Run
`agentctl tui` for the same operations in a terminal UI.

## Going further

- A repository checkout at boot needs an image with an init system and a boot-time service, like
  [`agents/self-dev`](../../../agents/self-dev).
- Access to GitHub or another service needs a mediated secret: the token stays on the host, and the network mediator
  substitutes it into requests to the hosts the manifest allows.
- Variants such as `agent.nested.yaml` extend `agent.yaml` and change only what differs.
