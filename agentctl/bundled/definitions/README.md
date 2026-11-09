# Bundled agent definitions

Each folder here is an agent definition that agentd carries in its build, named by the folder: an `agent.yaml` and
the files it names. They are for agents the platform runs for itself, such as the one that signs in to a harness,
whose definitions are tied to the code of the build that runs them, so they cannot come from anywhere else.

Otherwise they are ordinary definitions. Their reference is `bundled/<folder>`, or the folder's name for short. agentd
writes each to a folder of its own, and `agentctl run NAME` and `agentctl apply --from NAME` use that folder as they
would any other. [`agent::bundled`](../../src/bundled/mod.rs)
checks them when they are built:

- Paths, such as `spec.home.source`, name files in the definition's folder, since nothing else comes with it.
- It has no bind mounts, since the host it runs on is not known. One that works on a folder of the person's declares
  `spec.workspace`, and they name the folder when they run it.
- It reads nothing from an environment file, so it declares no `spec.environment` or `spec.secrets`.
- Its image names a base image by its `main` tag, such as `ghcr.io/digdir/agentctl-base:ubuntu-26.04`. A released
  agentd runs that image as its own release tagged it, so a definition always runs on the image built with it.
