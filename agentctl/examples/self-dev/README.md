# Agent platform self-development Agent

This Agent develops the Agent platform itself: `agentctl/`, `sandbox/` and the Rust workspace in digdir-agents.

| Variant | Checkout | Resources |
| --- | --- | --- |
| default (`agent.yaml`) | Fresh `digdir/digdir-agents` clone made at boot | Normal |
| `nested` | Fresh clone | Reduced to fit inside the default Agent |
| `worktree` | Current host checkout mounted read-write | Normal |

Every variant builds the directory's `Dockerfile` locally. Self-development images are not published to GHCR.

```sh
make user-install
agentctl claude login
cd agentctl/examples/self-dev
mkdir -p ~/.agent
cp .env.sample ~/.agent/self-dev.env
agentctl apply --env-file ~/.agent/self-dev.env --wait
agentctl attach session/s1
```

For the reduced nested variant:

```sh
agentctl apply --variant nested --env-file ~/.agent/self-dev.env --wait
agentctl create session/s1 --variant nested --harness codex
```

The worktree variant requires an environment file outside the mounted checkout:

```sh
agentctl apply --variant worktree --env-file ~/.agent/self-dev.env
```

Ignored local variants such as `agent.mine.yaml` may extend another sibling variant. Keep their credentials outside
the mounted checkout.

Inside a running Agent, `instructions.md` tells the harness how to build, test and run the platform nested, and the
`pr-evidence` skill how to record `agentctl` demonstrations and attach them to pull requests.
