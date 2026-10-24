---
name: agentctl
description: Run nested Agents from inside this Agent with agentctl. Covers installing it, logging its agentd in through this Agent's mediated credentials, applying a manifest, creating and prompting Sessions, reading their turns, and cleaning up. Use when a task needs a separate Agent, such as trying an Agent image or manifest or exercising agentctl end to end. Not for subagents, which are your harness's own and need none of this.
---

# Nested Agents

A nested Agent is a separate Sandbox VM, run by an `agentd` inside this Sandbox. The Agent platform section of your
instructions says whether this Agent can run them.

## 1. Install agentctl

```sh
command -v agentctl || curl -fsSL https://raw.githubusercontent.com/digdir/digdir-agents/main/agentctl/install.sh | sh
```

That installs the latest release in `~/.local/bin`. When you are changing agentctl itself, run `make user-install` from
your checkout instead, so the nested Agent runs your build. `agentctl` starts its own `agentd` on first use.

## 2. Log in

```sh
agentctl claude login   # in an Agent with Claude Code
agentctl codex login    # in an Agent with Codex
```

Inside an Agent, each stores this Agent's mediated placeholder rather than signing in, so the nested `agentd` never
holds a real credential. Run them before `apply`; a nested Agent that declares a harness optional leaves it out until
its login exists, then installs it within seconds.

## 3. Write the nested Agent's environment file

A manifest's `environment` and `secrets` come from an env file. Fill it from this Agent's own variables: its secrets
hold inert placeholders that chain through this Agent's mediation. Keep it outside every checkout.

```sh
printf 'GITHUB_TOKEN=%s\nGIT_USER_NAME=%s\nGIT_USER_EMAIL=%s\n' \
  "$GITHUB_TOKEN" "$GIT_USER_NAME" "$GIT_USER_EMAIL" > ~/nested.env
```

Never put a real secret value in it, and never print one.

## 4. Apply

```sh
agentctl apply -f path/to/agent.yaml --env-file ~/nested.env --wait
# or, from the manifest's directory, a sibling variant such as agent.nested.yaml:
agentctl apply --variant nested --env-file ~/nested.env --wait
```

- **Size:** it must fit inside this Sandbox beside your own work. Check `nproc` and `free -g`, and keep its `cpu` and
  `memory` well below them; Agents often ship a `nested` variant sized for this.
- **Image:** `type: build` builds with Podman here and takes minutes; `type: reference` pulls.
- **Name:** `--name` applies a second copy of one manifest.
- **Source directory:** an Agent stays bound to the directory it was first applied from. To apply it from another,
  such as another worktree, delete it first.

When `--wait` fails or stalls, `agentctl describe agent/NAME` shows its conditions.

## 5. Use it

```sh
agentctl create session/s1 --agent NAME --harness claudeCode --model haiku --effort low
agentctl prompt session/s1 --agent NAME --wait --prompt 'Run hostname and report it.'
agentctl turns session/s1 --agent NAME --last 1
agentctl exec agent/NAME -- systemctl is-system-running
```

- `--harness` is `claudeCode`, `codex` or `pi`. Test with the cheapest model and the lowest effort the harness offers.
- `prompt --wait` returns when the turn completes; read its result with `turns`.
- `exec` runs a command directly, for checking what a Session did.
- `attach` and `tui` need an interactive terminal; avoid them.
- `agentctl get agents` and `agentctl get sessions --agent NAME` list what exists.

## 6. Clean up

```sh
agentctl delete agent/NAME
```

Delete every nested Agent you created once you are done, and before removing the directory it was applied from.
`agentctl stop agent/NAME` and `agentctl start agent/NAME` keep its disk and Sessions when you need it again later.
