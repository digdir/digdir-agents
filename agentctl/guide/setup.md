# Setting up an agent

You are helping a person set up an agentctl agent: a coding agent that runs in a sandboxed Linux VM on their
machine, with its own disk, network mediated by the host, and no real credentials inside. Your job is to find out
what they need, write the agent's folder, start it and check that it works. They may never have written a
manifest or a Dockerfile; you will.

## 1. Find out what you can before asking

- `agentctl status --json`: agentd, the harness logins agents can use, the agent this directory already has, and
  how many agents the machine runs. `agentctl guide status` describes the object, and `agentctl get agents` lists
  them by name.
- The machine: its cores, memory and free disk (`sysctl -n hw.ncpu hw.memsize` and `df -h ~` on macOS, `nproc`,
  `free -g` and `df -h ~` on Linux), and whether `docker info` answers. Building an agent's image needs a running
  Docker Engine; Docker Desktop and Colima both work. If it does not answer, ask the person to start one.
- The repository: its README and AGENTS.md, its build files and version files (such as `.nvmrc`, `package.json`,
  `global.json`, `go.mod`, `rust-toolchain.toml`, `pyproject.toml`), CI workflows, and its Git remote.
- `agents/` at the repository root: an existing agent folder is a starting point, not something to overwrite.

## 2. Ask about goals, in plain words

Ask only what the person decides and you cannot find out. Usually:

- What should the agent work on, and which other repositories does that touch?
- Should it work on its own clone (safer: their checkout is untouched) or on their checkout mounted from this
  machine (its edits change their files directly)?
- Does it need to push to GitHub or clone private repositories? Then it needs a GitHub token.
- Which coding agent should it run? Propose the one you are (`claudeCode`, `codex` or `pi` in the manifest).
- Does it need to run containers, test web apps in a browser, or use a desktop they can watch?
- Anything it must not do, or anything they always want it to do?

Propose answers from what you found, and let them correct you. Don't explain file layouts or YAML unless asked.

## 3. Write the agent's folder

At the repository root, `agents/<name>/` (name: short, lowercase letters, digits and hyphens, such as `dev`, and
not already used by another agent on the machine):

| File | What goes in it | Details |
| --- | --- | --- |
| `agent.yaml` | The manifest: image, size, harnesses, mounts, secrets | `agentctl guide manifest` |
| `Dockerfile` | `FROM` the agentctl base image, plus what the work always needs | `agentctl guide image` |
| `instructions.md` | What the agent is for, and the tools and repositories it should have | `agentctl guide instructions` |
| `.dockerignore` | `*` unless the Dockerfile copies files from the folder | |
| `.env` | Values the manifest's `environment` and `secrets` name; never committed, and outside the repository when the agent mounts it | `agentctl guide manifest` |

Bake into the image what is large or always needed (a language toolchain, a database client). List every tool and
repository in `instructions.md` either way: the agent inside checks that list and installs or clones what is
missing, so the list is what keeps it working when the image falls behind.

Size the agent from the machine: up to half its cores, a quarter of its memory, and a disk that fits its free space.
Other agents on the machine share them.

Write the non-secret values in `.env` yourself, such as `GIT_USER_NAME` and `GIT_USER_EMAIL` from `git config`.
Never ask for a secret in the conversation and never read one: tell the person which line to add to the file, such as
`GITHUB_TOKEN=`, and make sure Git ignores the file.

## 4. Start it and check it

Check that the harness the agent uses has a login: in `agentctl status --json`, its `login_stored` is `true`. If
not, ask the person to run `agentctl claude login` or `agentctl codex login` in their own terminal; it opens a
browser. Then, from the repository root, adding `--env-file <path>` when `.env` is not beside the manifest:

```sh
agentctl apply -f agents/<name>/agent.yaml --wait --timeout 30m
agentctl status --json
agentctl exec agent/<name> -- node --version
```

`apply --wait` builds the image, boots the agent and prints its progress until it is ready; the first build can take
several minutes. It exits non-zero on failure: read the error, fix the files and apply again. `status --json` should
then show the agent with `state` `ready`; `agentctl describe agent/<name>` has the details when it is not. Use
`exec` to check that the tools you put in the image are there.

Applying again updates a running agent. The size changes in place, restarting the VM, and the disk can only grow.
The image, mounts, instructions file, init system and harness types cannot change: `apply` refuses with
`immutable Agent field changed` and the field. A changed Dockerfile is also only built when the agent is created. To
change those, run `agentctl delete agent/<name>`, wait until `agentctl get agents` no longer lists it, and apply
again. The agent's disk starts over, so ask the person first if it has work that is not pushed.

## 5. Hand over

Tell the person, in two or three lines, what you set up and how to start working with it: `agentctl attach
session/main` from anywhere in the repository starts a session named `main` and attaches to it (detach with
Ctrl-b d), and `agentctl tui` shows every agent and session. Suggest they commit `agents/<name>/`, without `.env`, so
others in the repository get the same agent.
