---
name: agentctl
description: Drive agents with agentctl, from a host session acting as orchestrator or from inside an agent running nested ones. Covers applying a manifest, creating and prompting sessions, reading their turns and cleaning up, and, inside an agent, installing agentctl and logging it in. Use when a task needs a separate agent, such as handing work to one, trying an agent image or manifest, or exercising agentctl end to end. Not for subagents, which are your harness's own and need none of this.
---

# Driving agents with agentctl

An agent is a sandbox VM whose harness sessions an `agentd` runs, and `agentctl` drives it. First work out where you
are:

- **Inside an agent:** your instructions have an agent sandbox section, or `AGENT_NAME` is set. The agents you start
  are nested in this sandbox, and that section says whether it can run them. Set up as below.
- **On a host:** neither is true. `agentctl`, its logins, the agents and their environment files are the user's.
  Follow the host rules below.

## Inside an agent: set up

**Install** `agentctl` unless it is on `PATH`. When you are changing agentctl itself, run `make user-install` from
your checkout instead, so the nested agent runs your build.

```sh
command -v agentctl || curl -fsSL https://raw.githubusercontent.com/digdir/digdir-agents/main/agentctl/install.sh | sh
```

**Log in** for each harness this agent has, before `apply`. They reuse this agent's login; for a harness it lacks,
they fail rather than sign in. A nested manifest whose harness names a `login` needs a login of that name here, such
as `agentctl claude login work`. For a list, such as `login: [work, private]`, one of them is enough, and `login: any`
takes any login stored here.

```sh
agentctl claude login
agentctl codex login
```

**Write the environment file** with the variables the manifest declares, from this agent's own, outside every
checkout:

```sh
printf 'GITHUB_TOKEN=%s\nGIT_USER_NAME=%s\nGIT_USER_EMAIL=%s\n' \
  "$GITHUB_TOKEN" "$GIT_USER_NAME" "$GIT_USER_EMAIL" > ~/nested.env
```

**Size** a nested agent to fit inside this sandbox beside your own work. Disk usually runs out first: the nested
agent's disk can grow to its `rootFilesystem.capacity`, and every image built for one stays in Podman's storage. Check
`df -h /`, `nproc` and `free -g`, and keep its `capacity`, `cpu` and `memory` well below them; agents often ship a
`nested` variant sized for this.

## On a host: rules

- The agents already there are the user's work. Change only those you create or the user names, and never run
  `stop --all` or `kill --all`.
- Logins are the user's. When `apply` or `create` reports one missing, ask the user to run the login command it names,
  such as `agentctl claude login work`; do not sign in yourself.
- Environment files hold real secrets: never read, write or print one. When the manifest needs values and there is
  no env file to use, as below, ask the user which to pass.

## Apply

```sh
agentctl apply -f path/to/agent.yaml --env-file ENV_FILE --wait
# or, from the manifest's directory, a sibling variant such as agent.nested.yaml:
agentctl apply --variant nested --env-file ENV_FILE --wait
```

- **Name:** `apply` updates the agent of the manifest's name, and refuses one first applied from another directory.
  Others may share this `agentctl`, such as the user on a host or other sessions in an agent: check
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
- `exec` runs a command directly, for checking what a session did.
- `attach` and `tui` need an interactive terminal; avoid them.
- `agentctl get agents` and `agentctl get sessions --agent NAME` list what exists.

## Clean up

`agentctl delete agent/NAME` removes an agent with its disk and sessions, for good. `agentctl stop agent/NAME` keeps
both, and `agentctl start agent/NAME` resumes it.

- **Inside an agent:** delete the nested agents you applied once you are done, and before removing the directory one
  was applied from. Images built for them stay in Podman's storage: `podman system df` shows its size, and
  `podman image ls` and `podman rmi` remove those no longer needed.
- **On a host:** stop rather than delete, unless the user asks you to delete.
