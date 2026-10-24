---
name: agentctl
description: Drive Agents with agentctl, from a host session acting as orchestrator or from inside an Agent running nested ones. Covers applying a manifest, creating and prompting Sessions, reading their turns and cleaning up, and, inside an Agent, installing agentctl and logging it in. Use when a task needs a separate Agent, such as handing work to one, trying an Agent image or manifest, or exercising agentctl end to end. Not for subagents, which are your harness's own and need none of this.
---

# Driving Agents with agentctl

An Agent is a Sandbox VM whose harness Sessions an `agentd` runs, and `agentctl` drives it. First work out where you
are:

- **Inside an Agent:** your instructions start with `# Agent sandbox`, or `AGENT_NAME` is set. The Agents you start
  are nested in this Sandbox, and that section says whether it can run them. Set up as below.
- **On a host:** neither is true. `agentctl`, its logins, the Agents and their environment files are the user's.
  Follow the host rules below.

## Inside an Agent: set up

**Install** `agentctl` unless it is on `PATH`. When you are changing agentctl itself, run `make user-install` from
your checkout instead, so the nested Agent runs your build.

```sh
command -v agentctl || curl -fsSL https://raw.githubusercontent.com/digdir/digdir-agents/main/agentctl/install.sh | sh
```

**Log in** for each harness this Agent has, before `apply`. They reuse this Agent's login; for a harness it lacks,
they fail rather than sign in.

```sh
agentctl claude login
agentctl codex login
```

**Write the environment file** with the variables the manifest declares, from this Agent's own, outside every
checkout:

```sh
printf 'GITHUB_TOKEN=%s\nGIT_USER_NAME=%s\nGIT_USER_EMAIL=%s\n' \
  "$GITHUB_TOKEN" "$GIT_USER_NAME" "$GIT_USER_EMAIL" > ~/nested.env
```

**Size** a nested Agent to fit inside this Sandbox beside your own work. Disk usually runs out first: the nested
Agent's disk can grow to its `rootFilesystem.capacity`, and every image built for one stays in Podman's storage. Check
`df -h /`, `nproc` and `free -g`, and keep its `capacity`, `cpu` and `memory` well below them; Agents often ship a
`nested` variant sized for this.

## On a host: rules

- The Agents already there are the user's work. Change only those you create or the user names, and never run
  `stop --all` or `kill --all`.
- Logins are the user's. When `apply` or `create` reports one missing, ask the user to run `agentctl claude login` or
  `agentctl codex login`; do not sign in yourself.
- Environment files hold real secrets: never read, write or print one. When the manifest needs values and there is
  no env file to use, as below, ask the user which to pass.

## Apply

```sh
agentctl apply -f path/to/agent.yaml --env-file ENV_FILE --wait
# or, from the manifest's directory, a sibling variant such as agent.nested.yaml:
agentctl apply --variant nested --env-file ENV_FILE --wait
```

- **Name:** `apply` updates the Agent of the manifest's name, and refuses one first applied from another directory.
  Others may share this `agentctl`, such as the user on a host or other Sessions in an Agent: check
  `agentctl get agents` first, and pass `--name` to give yours a name of its own.
- **Env file:** without `--env-file`, the first `apply` uses `.env` beside the manifest, and later ones the file it
  recorded.
- **Image:** `type: build` builds the image first and takes minutes; `type: reference` pulls it.

When `--wait` fails or stalls, `agentctl describe agent/NAME` shows its conditions.

## Use it

```sh
agentctl create session/s1 --agent NAME --harness claudeCode --model haiku --effort low
agentctl prompt session/s1 --agent NAME --wait --prompt 'Run hostname and report it.'
agentctl turns session/s1 --agent NAME --last 1
agentctl exec agent/NAME -- systemctl is-system-running
```

- `--harness` is `claudeCode`, `codex` or `pi`. For trials, use the cheapest model and the lowest effort it offers.
- `prompt --wait` returns when the turn completes; read its result with `turns`.
- `exec` runs a command directly, for checking what a Session did.
- `attach` and `tui` need an interactive terminal; avoid them.
- `agentctl get agents` and `agentctl get sessions --agent NAME` list what exists.

## Clean up

`agentctl delete agent/NAME` removes an Agent with its disk and Sessions, for good. `agentctl stop agent/NAME` keeps
both, and `agentctl start agent/NAME` resumes it.

- **Inside an Agent:** delete the nested Agents you applied once you are done, and before removing the directory one
  was applied from. Images built for them stay in Podman's storage: `podman system df` shows its size, and
  `podman image ls` and `podman rmi` remove those no longer needed.
- **On a host:** stop rather than delete, unless the user asks you to delete.
