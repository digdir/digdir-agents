# agentctl

`agentctl` runs long-running agents with durable sessions in isolated [sandboxes](../sandbox/), with mediated network
access and no real secrets inside the sandbox.

It is a toolkit for building an agent's computer rather than a fixed workflow. The CLI, the TUI and tmux are one way
in. Any client that connects over SSH, such as a desktop coding app or an editor, works as well.

## Install

Linux and macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/digdir/digdir-agents/main/agentctl/install.sh | sh
```

Windows, from PowerShell, with the `HypervisorPlatform` optional feature enabled:

```powershell
irm https://raw.githubusercontent.com/digdir/digdir-agents/main/agentctl/install.ps1 | iex
```

Log in to the harnesses your agents use:

```sh
agentctl claude login
agentctl codex login
```

To keep more than one subscription, such as one for work and one of your own, store each under a name with
`agentctl claude login work`, and set `login: work` on the harness in the manifests that should use it. With
`login: [work, private]` or `login: any`, each agent gets whichever of them the fewest other running agents use, and
keeps it. `agentctl claude list` shows the logins and the agents that use each.

Update with `agentctl self update`. Agents and sessions carry over.

## Your first agent

`agentctl setup` asks a few questions and leaves you with a running agent that is yours. Your choices go in a variant
of the agent's manifest of your own beside it, `agent.mine.yaml` unless you name another, so the definition stays as
its makers keep it, and their changes keep reaching your agent.

- **Start from an agent your repository defines.** Run in a checkout, setup offers the agents it defines, as
  `agentctl tui` does. It writes your variant beside the manifest you pick and applies it there, so the agent's home,
  instructions and skills follow the checkout, and a `git pull` brings the team's changes. Git must ignore the
  variant, as `agent.*.yaml` in the folder's `.gitignore` does. Pick your variant later to change it.
- **Start from an agent definition elsewhere**: a manifest or a folder on disk, or a GitHub address such as
  `https://github.com/Altinn/altinn-studio//agents/minimal`, or the address GitHub shows for a folder or a manifest.
  Setup downloads it, with your `git` when you have it, else as GitHub's archive of a public repository, and keeps it
  in agentctl's home, such as `~/.agent/sources/github.com/Altinn/altinn-studio/`, since the agent reads its home,
  instructions and skills from there. Running setup on the address again downloads it again and keeps your variant.
  An agent that works on a checkout of its repository, as a worktree variant does, gets a clone in a folder you
  choose (`--clone-into`), or uses the checkout already there.

The recommended answers usually fit, so setup fills them in and shows them before it writes anything: a name, a size
measured against your machine (or the CPUs, memory and disk you choose, also with `--cpus`, `--memory-gib` and
`--disk-gib`), the manifest's harnesses and its default one, the folders it shares, how the agent reaches GitHub and
who its commits are from, and which optional values stay empty. Choose Change something to answer
each question yourself. Setup still asks for what has no default, such as a required token. The review also opens your
variant in your editor, for anything the questions leave at its recommended value, such as the model or hosts the agent
may not reach. Change it later and run `agentctl apply -f agent.mine.yaml` beside it.

- **Values** the manifest declares, such as tokens, go in a file in agentctl's home, such as `~/.agent/<name>.env`,
  outside any checkout. agentd remembers it, so a later `apply` reads it too. When an agent needs several, you can
  fill them in all at once in your editor (`--edit-values`) instead of answering a question for each, and the review
  opens them again.
- **Shared folders** are folders on your machine that you and the agent both work on, so what it changes lands in
  them. Without one, the agent works on its own copies, which you reach over SSH. Setup lists what the
  manifest shares and lets you keep it, share another folder (`--mount`), or leave one out (`--unshare`). It never
  gives an agent your settings and credentials: not your whole home folder, the folders in it whose name starts with a
  dot, `~/Library` or `AppData`, nor agentctl's own state. That covers what a definition shares, and its
  home, instructions and skills from outside its repository.
- **Logins** agentctl holds are used, and when you keep several, setup asks which the agent uses (`--login`). It signs
  you in to those missing as `agentctl claude login` does. When the manifest declares `GITHUB_TOKEN`, setup signs in to
  GitHub through the browser in a short-lived agent, so this machine needs no `gh`, or checks a token you paste.
- **Questions** take the arrow keys and Enter, and Esc goes back to the question before. Every question has a flag,
  and setup ends by printing the command that repeats it without questions, so a script runs the same setup without
  a terminal:

```sh
agentctl setup --non-interactive --yes --from https://github.com/Altinn/altinn-studio//agents/minimal --github-token-stdin < token.txt
```

To write an agent of your own, start from the [.NET example](../agents/examples/dotnet-dev/), an [agent definition](../GLOSSARY.md) whose manifest
and image explain each line. Let an agent make it yours: ask Claude Code or Codex to copy it and change it for the
tools and repositories your work needs. A coding agent working in an agent can apply what it writes as a nested agent
and try it before you do.

## Documentation

- [docs/](docs/README.md): agentctl's own docs, such as what an agent image must provide
- [architecture.md](../docs/architecture.md): the principles and diagrams
- [GLOSSARY.md](../GLOSSARY.md): the terms used here
- [CHANGELOG.md](CHANGELOG.md): release notes
- [CONTRIBUTING.md](../CONTRIBUTING.md): development
