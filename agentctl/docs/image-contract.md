# Agent image contract

What `agentd` expects of the image an agent runs. Build on the
[agentctl base images](../../images/agentctl-base/README.md) and end the build with `agent-image-check`, which checks
the contract and explains each check in its
[source](../../images/agentctl-base/headless/usr/local/bin/agent-image-check).

`agent-image-check` cannot check the following:

- **Harness versions.** The adapters are tested with the versions the base images pin, so keep those. A harness
  `version` declared in the manifest must match the installed one exactly.
- **Podman.** With Podman installed, systemd must run and provide `podman.socket`. `agentd` configures the socket and
  Podman's trust itself. The base images' `containers` feature installs Podman this way.
- **Access.** `ssh` and `vnc` access need `initSystem: image` with systemd as the entrypoint.
- **Claude Code's first-run state.** `~/.claude/.claude.json`, with onboarding and trust settled, comes from the image.
  The base images ship it.

`agentd` writes the harnesses' configuration, instructions and skills, and its own setup, each time it reconciles the
agent. It leaves other files alone, so an image may ship skills of its own.
