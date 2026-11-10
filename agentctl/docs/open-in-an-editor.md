# Open an agent in an editor

Open an agent's project in VS Code, Zed or another editor that works over SSH. The editor runs on your machine, and
its files, terminal and tools are the agent's. For a JetBrains IDE, see
[Open an agent in a JetBrains IDE](open-in-jetbrains.md).

The agent needs `access` with `type: ssh` in its manifest, as the [.NET example](../../agents/examples/dotnet-dev/)
has, and must be `Ready`.

These steps are what one command, `agentctl connect NAME --client vscode`, is meant to do for you: set up SSH, start
the agent, choose the project and open the editor. That command doesn't exist yet.
[#386](https://github.com/digdir/digdir-agents/issues/386) proposes it.

## Set up SSH once

```sh
agentctl ssh-config install
```

This adds one `Include` line at the top of `~/.ssh/config`. The included file reaches every agent with SSH access as
`agentctl-NAME`, and `agentd` keeps it current as agents come and go.

## Open it from the TUI

```sh
agentctl tui
```

Select the agent and press `o`. Then press `c` for VS Code or `z` for Zed. The editor opens the agent's project, the
folder `ssh-info` names as `Project`.

If SSH isn't set up yet, the agent's panel says `SSH not set up; o offers it`, and the menu ends with "Set up SSH".
Choose it and press `Enter` to add the line, as `agentctl ssh-config install` does.

VS Code needs the [Remote - SSH](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-ssh)
extension. Without its command, `code`, on your `PATH`, the TUI opens VS Code through a `vscode://` link instead. Zed
needs its command, `zed`, on your `PATH`, and the menu says so when it isn't.

## Open it from the command line

Find the alias and the project:

```sh
agentctl ssh-info agent/NAME
```

For the agent `altinn-profile` from [the tutorial](first-agent.md), the output includes:

```text
Alias:       agentctl-altinn-profile
User:        agent
Directory:   /home/agent/code
Project:     /home/agent/code/altinn-profile
```

Then open `Project` with `Alias`:

```sh
code --remote ssh-remote+agentctl-altinn-profile /home/agent/code/altinn-profile
```

```sh
zed ssh://agentctl-altinn-profile/home/agent/code/altinn-profile
```

## The first connection

Each editor installs a server of its own in the agent the first time it opens it:

- **VS Code** first asks for the platform of the remote host. Choose Linux. It asks once for each agent and keeps the
  answer in its `remote.SSH.remotePlatform` setting. Until you answer, the window waits at "Install and start server
  if needed". The server then takes a few seconds to install, and about 700 MB of the agent's disk. Opening a folder
  that a VS Code window already shows brings that window forward instead of opening another.
- **Zed** puts its remote server, about 110 MB, in the agent, and opens the project in about half a minute.

Later connections reuse the server, which stays in the agent's home until you delete the agent.

## Open it in another editor

Any editor that connects to SSH hosts from `~/.ssh/config` works the same way: connect to the host `agentctl-NAME`
and open the folder `ssh-info` names as `Project`. A terminal reaches the agent with `ssh agentctl-NAME`, or with
`agentctl ssh agent/NAME` without the setup.

## When the editor loses the agent

The editor's connection ends when the agent's computer stops, such as with `agentctl stop`, `agentctl restart`, or an
`apply` that changes its resources. Reconnect from the editor once `agentctl get agents` shows the agent as `Ready`.
When the agent is deleted, its alias leaves the included file, and the editor reports `Could not resolve hostname`.
Close that window.

## When the folder isn't there

`Project` is the repository the agent's image clones when there is exactly one, and `/home/agent/code` otherwise. The
agent clones while it starts, so the folder can be missing for a minute after the agent turns `Ready`. If it stays
missing, the clone failed. List what is there:

```sh
agentctl ssh agent/NAME -- ls /home/agent/code
```

The clone's log says why it failed:

```sh
agentctl ssh agent/NAME -- sudo journalctl -u agent-workspace-init
```

The agent clones with the GitHub CLI, which needs a GitHub token even for a public repository: the login the
manifest's `accounts` names, such as `agentctl github login default`, or `GITHUB_TOKEN` in the agent's `.env`.
Without it, the log says `To get started with GitHub CLI, please run: gh auth login`. Add the token, apply the
agent again and restart it with `agentctl restart agent/NAME`, which clones what is missing as it starts.
