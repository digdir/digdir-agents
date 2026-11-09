# Bundled agent definitions

The agents agentd runs for itself, such as the one that signs in to a harness, come from definitions agentd carries in
its build, since they are tied to the code of that build. Each folder here is one, named by the folder: an `agent.yaml`
and the files it names. Clients start one with the control API's `agents.v1.ensure`, and
[`agent::bundled`](../../src/bundled/mod.rs) lists how agentd runs each of them.

A bundled definition differs from one on disk in what it may name, since the agent reads it from agentd's copy rather
than from a folder of yours:

- Paths, such as `spec.home.source`, name files in the definition's folder.
- It has no bind mounts. agentd binds the folder the client names as the workspace, if the definition takes one.
- It reads nothing from an environment file, so it declares no `spec.environment` or `spec.secrets`.
- Its image names a base image by its `main` tag, such as `ghcr.io/digdir/agentctl-base:ubuntu-26.04`. A released
  agentd runs that image as its own release tagged it, so a definition always runs on the image built with it.
