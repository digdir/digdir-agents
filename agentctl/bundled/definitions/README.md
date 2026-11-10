# Bundled agent definitions

Each folder here is an agent definition that agentd carries in its build, named by the folder: an `agent.yaml` and
the files it names. They are for agents that are part of the platform, such as the agent builder `agentctl new` runs,
whose definitions are tied to the code of the build that runs them, so they cannot come from anywhere else.

Otherwise they are ordinary definitions. Their reference is `bundled/<folder>`, or the folder's name for short. agentd
writes each to a folder of its own when it starts, so it holds the version that build carries, and `agentctl run NAME`
and `agentctl apply --from NAME` use that folder as they would any other. `agentctl get definitions` and the TUI list
them after the definitions on disk. [`agent::bundled`](../../src/bundled/mod.rs) checks them when they are built:

- Paths, such as `spec.home.source`, name files in the definition's folder, since nothing else comes with it.
- Its bind mounts name no source, since the host it runs on is not known. One that works on a folder of the person's
  leaves the source out, and they name the folder when they run it.
- It reads nothing from an environment file, so an entry of `spec.environment` gives its `value`, and one of
  `spec.secrets` names a secret stored on the machine with `name`.
- Its image is one each release publishes, named by its `main` tag, such as `ghcr.io/digdir/agentctl-base:ubuntu-26.04`
  or the agent builder's `ghcr.io/digdir/agentctl-agent-builder:main`. A released agentd runs that image as its own
  release tagged it, so a definition always runs on the image built with it. A new image needs adding to
  `RELEASED_IMAGES` in [`agent::bundled`](../../src/bundled/mod.rs) and to the release workflow.
