---
name: agent-definition
description: Write or change an agent definition, the files an agentctl agent is made from (agent.yaml and its variants, the image's Dockerfile, instructions, skills and home), and try the change in a nested agent. Use when making an agent for a repository or a role, or when improving the definition of the agent you run in.
---

# Agent definitions

An [agent definition](https://github.com/digdir/digdir-agents/blob/main/GLOSSARY.md) is the folder an agent is made
from. The agent it makes should do the whole job of its role on its repository, the way a teammate would. A developer
builds, runs the unit, integration and end-to-end tests, browser tests included, and shows the result. A designer runs
the app and captures it. A documentation writer builds and previews the docs. What the agent cannot run, its
instructions say, so that it reports it instead of working around it.

The sources of truth are in [digdir-agents](https://github.com/digdir/digdir-agents): the manifest schema in
[manifest.rs](https://github.com/digdir/digdir-agents/blob/main/agentctl/src/manifest.rs), the commented
[.NET example](https://github.com/digdir/digdir-agents/tree/main/agents/examples/dotnet-dev) and the
[base images](https://github.com/digdir/digdir-agents/blob/main/images/agentctl-base/README.md). Read them rather than
guessing: the manifest refuses unknown fields.

## Where a definition lives

- **A team's agent** lives in the repository it works on, in `agents/<name>/`, so it changes with the code it serves
  and through the same review. `agentctl tui` and `agentctl apply` find definitions anywhere in the repository they
  run in.
- **A personal agent** lives in `~/agents/<name>/`, under Git. Outside a repository, agentctl finds definitions only
  in `agents/<name>/` of the folder it runs in, so this is where `agentctl tui` and `agentctl apply` run from the
  home directory find it.

Either way, its owner makes the agent theirs with a variant beside the manifest, such as `agent.mine.yaml`, which
holds their own choices, and applies it with `agentctl apply --variant mine`. Give the folder a `.gitignore` that
keeps such variants out of Git, and leave them alone.

## Shape

A team's agent has a `minimal` stage and, when the role needs a browser or a desktop, a `desktop` stage built on it.
Each stage is an agent of its own, so it gets a folder with its manifest, its variants and what is particular to its
machine. What the stages share sits once beside them:

```
agents/<name>/
├── Dockerfile        one image with a stage each, so a host that has minimal pulls only what desktop adds
├── .dockerignore
├── common/           what every stage installs, such as boot units, workspace.list and the scripts they run
├── instructions.md
├── improving.md
├── skills/<skill>/
├── home/             copied over /home/agent, such as harness settings
├── minimal/
│   ├── agent.yaml
│   ├── agent.nested.yaml, agent.nested-build.yaml, agent.worktree.yaml
│   ├── environment.md
│   └── .gitignore
└── desktop/          the same, for the desktop stage
```

A personal agent is one folder with its manifest, variants, a `Dockerfile` with one stage, instructions and a
`.gitignore`.

- **Image:** a team's manifests reference the image its CI publishes (`type: reference`), so teammates need no Docker.
  The workflow builds each stage on native amd64 and arm64 runners and publishes `latest`.
  A personal agent builds its image on its owner's machine (`type: build`), or references an image published for
  another agent when that has its tools.
- **Runtime files stay out of the image:** a `.dockerignore` that admits only what the Dockerfile copies. Instructions,
  skills and the home overlay come from the manifest, so they change without a rebuild.
- **`.gitignore`:** `agent.*.yaml` except the variants below, so personal variants stay out of Git.

## The image

Build on the base images and add the features the role needs, as their README describes: `containers` for services
the tests start with Compose, `browser` for browser tests, `desktop` for the desktop and computer use.

- **Pin each tool to the version the repository pins**, in `global.json`, `.nvmrc`, `rust-toolchain.toml`, `go.mod`
  or its CI workflows, and check each download's checksum, so the agent gets the results CI gets.
- **Install outside `/home/agent`.** agentd copies the home overlay over it on every reconcile, and a cache a root
  step leaves there locks the agent out of it.
- **Trust the mediated CA in build steps** that download through Node or Python, as the base images' README says,
  without persisting it with `ENV`: every request the agent makes goes through the host.
- **End with `agent-image-check`**, which checks the image contract and explains each failure.
- **Boot-time setup**, such as a login with a mediated key, hosts entries or a container network, runs as a systemd
  oneshot unit, since the values exist only once the agent runs. Repositories to clone go in
  `/etc/agent-workspace.d/*.list`.
- **For nested agents**, add `containers`, copy `agentctl` and `agentd` in from `ghcr.io/digdir/agentctl`, and declare
  `nestedSandboxes: preferred`. That lets the agent try changes to its own definition, as below.

## The manifest

- **Values come from the machine the agent runs on, not from a file beside the manifest.** A later agentctl stops
  reading the environment file, `.env`, as
  [moving off the environment file](https://github.com/digdir/digdir-agents/blob/main/agentctl/docs/moving-off-the-environment-file.md)
  explains, so write no `.env` or `.env.sample`, even where older agents have one:
  - **GitHub:** an entry of `accounts` with `type: github` and a `login`, such as `default`, gives the agent a
    `GITHUB_TOKEN` for GitHub's hosts and commits as that account. Its owner signs in with
    `agentctl github login default`.
  - **Secrets** stay on the host, stored under a name with `agentctl secret set NAME --hosts HOST`. An entry of
    `secrets` names one and the variable it goes in. Give it the narrowest `allowedHosts` its service needs, and
    `optional` when the agent can work without it.
  - **Plain values** that are the same for everyone go in `environment` with `value`. The agent sees them as they
    are, so never a secret.
- **Network:** `mediated`, allowing all, and denying `metadata.google.internal`, which hands out the credentials of
  the machine the host may run on.
- **Harnesses:** Claude Code as the default and Codex as optional, unless the role needs others. `optional` lets the
  agent start without a login the host lacks.
- **Volumes:** when agents need a large host directory they only read, a `kind: Volume` manifest copies it once for
  all of them, and each mounts it with `type: volume`. Keep a team's volume manifests in `agents/_volumes/`, where
  `agentctl tui` applies them.

## Instructions and skills

- **Instructions** say what the agent is for and where it works, and point at the repository's own `AGENTS.md`,
  `CONTRIBUTING.md` and READMEs for how to build and test, rather than restating them, which goes stale.
- **`environment.md`**, per stage, says what that machine can and cannot run and the traps in it, such as a tool that
  needs a flag to find the browser.
- **`improving.md`** says where the agent's own definition is and that this skill applies to it.
- **Skills** hold procedures only some tasks need, such as what this repository's pull requests show as evidence. The
  base images give every agent a `recording` skill for recording a terminal, a page or the desktop, so a skill of the
  agent's own says what to record and where it goes, not how.

## Variants

- **`nested`** runs the same image inside another agent, sized to fit there, such as 2 CPUs, 3Gi and 24Gi, with
  `nestedSandboxes: null`.
- **`nested-build`** extends `nested` and builds the image from the checkout (`type: build`), so a change to the
  Dockerfile can be tried before CI publishes it.
- **`worktree`**, for a team's agent, mounts the developer's own checkout instead of a fresh clone: the repository's
  root, three folders up from the stage's (`source: ../../..`), at the path its workspace list clones it to. The base
  image leaves a checkout it finds there alone.

## Trying a change

1. Apply the `nested-build` variant from the checkout that holds the change, as the `agentctl` skill describes.
2. In the nested agent, run what its instructions promise: the build, the unit, integration and end-to-end tests,
   browser tests on a desktop stage. Prompt one session there with the cheapest model, to check that the harness, its
   instructions and its skills work.
3. Report each capability as verified, or not and why. Delete the nested agent.

## What an update can change in place

agentd fixes, when it creates an agent, the fields `validate_immutable_fields` in
[service.rs](https://github.com/digdir/digdir-agents/blob/main/agentctl/src/control_plane/service.rs) checks, such as
the image, the mounts, the home overlay and its harnesses. Changing one takes creating the agent again, which deletes
its disk, so `agentctl apply` refuses it until the agent is deleted. Instructions and skills, the size, the harness
defaults, secrets and network change in place. A new published image reaches an agent only when it is created again.

## Proposing a change

- **A team's agent:** a branch and a pull request, saying what you verified and how.
- **A personal agent:** commit in its definition folder, and tell its owner to run `agentctl apply` there with their
  variant, which applies the change.
- **Say so when a change widens what the agent may reach**, such as hosts, mounts or secrets. That is its owner's
  decision, not yours.
