# Agent platform self-development Agent

This Agent develops the Agent platform itself: `agentctl/`, `sandbox/` and the Rust workspace in digdir-agents.

| Variant | Image | Checkout | Resources |
| --- | --- | --- | --- |
| default (`agent.yaml`) | Published | Fresh `digdir/digdir-agents`, `digdir/microsandbox` and `digdir/libkrunfw` clones made at boot | Normal |
| `nested` | Published | Fresh clones | Reduced to fit inside the default Agent |
| `nested-build` | Built from this checkout | Fresh clones | Reduced to fit inside the default Agent |
| `worktree` | Published | Current host checkout mounted read-write, fresh fork clones | Normal |

The published image, `ghcr.io/digdir/digdir-agents/agent-self-dev:latest`, is built from this directory's `Dockerfile`
whenever it changes on `main`. Use `nested-build` to try changes to the image itself.

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
`pr-evidence` skill how to record `agentctl` demonstrations and attach them to pull requests, and the `changelog`
skill how to write changelog entries.
