# The prelaunch script

Some setup has to be in place before a harness starts, because the harness reads it only at launch: its user
settings, rules, skills or memory. An Agent's manifest can name a script for that in `spec.prelaunch`, and `agentd`
runs it in the Agent's sandbox before each launch of a Session's harness: a new Session, a resumed one, and one woken
from idle. It does not run for `agentctl exec`.

```yaml
spec:
  prelaunch:
    source: prelaunch.sh
```

`source` resolves like the other files a manifest names: against the manifest's directory, or against your home
directory when it starts with `~/`. The field is optional. Without it nothing runs, nothing is recorded and a launch
takes no longer.

A script that only some people have, such as one in your own home directory, can be marked `optional`, so that the
manifest still works for colleagues without the file:

```yaml
spec:
  prelaunch:
    source: ~/agent-prelaunch.sh
    optional: true
```

Without `optional`, a missing file is an `error` outcome. With it, a missing file runs nothing, as if the field were
not there. A path that exists but is not a readable file is an `error` either way.

What needs no credentials or network belongs in the image's Dockerfile. The script is for setup that does, such as
cloning repositories or setting up a CLI.

## How it runs

- `agentd` reads the file at each launch, so an edit applies to the next launch without recreating the Agent.
- It runs with `sh` inside the Agent's sandbox, so it needs no executable bit and a `#!` line is ignored: to use bash,
  have it run a bash file, such as `exec bash ~/setup/prelaunch.bash`. It runs as `agent`, in `/home/agent`,
  with the environment the harness gets: the Agent's own environment and secret placeholders, the harness's own
  settings such as `CLAUDE_CONFIG_DIR` or `CODEX_HOME`, and the `AGENT_*` variables, such as
  `AGENT_NAME` and `AGENT_HARNESS` (`claudeCode`, `codex` or `pi`).
- It has the same credentials and network access as the Session: the mediated secret placeholders, the harness logins
  and whatever Git configuration `agentd` sets up in the sandbox. Do not write secrets into the script itself, since
  its content is visible in the sandbox's process list while it runs. Read them from the environment instead.
- It may run for 30 seconds. Then `agentd` kills it and launches the harness anyway.
- It never stops a launch. A script that fails, times out or cannot run is recorded, and the harness starts.
- `agentd` runs one Agent's prelaunch scripts one at a time, so two Sessions launching together never run it side by
  side in the same sandbox. Different Agents run theirs in parallel, each in its own sandbox.
- It must be UTF-8 text of at most 64 KiB. Fetch anything larger from it.
- `AGENT_HARNESS` tells it which harness is about to start, so it can skip harnesses its setup does not serve.

## What it reports

The harness gets the outcome as `AGENT_PRELAUNCH_STATUS`:

| Value | Meaning |
| --- | --- |
| `ok` | The script exited with 0. |
| `failed:<exit code>` | The script exited with another code, such as `failed:1`. |
| `timeout` | The script ran too long and was killed. |
| `error` | The script could not be run, for example because the file is missing or is not text. |

When no script ran the variable is not set. `agentctl get session/NAME -o json` shows the same value under
`status.prelaunch`, with the last 4 KiB of the script's output, its standard error included, until the next launch.

## Files it must leave alone

On every pass, about every 30 seconds, `agentd` writes these again, so a change the script makes to them is undone:

- whatever the Agent's home overlay (`spec.home`) holds;
- each harness's instructions file: `~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md` or `~/.pi/agent/AGENTS.md`;
- the skills the Agent's manifest declares, and the platform's `agentctl` skill;
- the files `agentd` configures each harness with: its credentials, its activity hooks and their configuration, and
  for Claude Code `~/.claude/agent-settings.json` and its status line.

Anything else in the home directory is the script's, such as `~/.claude/settings.json` when the home overlay has none.
Running Sessions may read a file while the script replaces it, so write a new version beside it and rename it into
place.
