# Open an agent in an editor

Open an agent's project in VS Code, Zed or another editor that works over SSH. The editor runs on your machine, and
its files, terminal and tools are the agent's. For a JetBrains IDE, see
[Open an agent in a JetBrains IDE](open-in-jetbrains.md).

The agent needs `access` with `type: ssh` in its manifest, as the [.NET example](../../agents/examples/dotnet-dev/)
has, and must be `Ready`.

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

Select the agent and press `o`. Then press `c` for VS Code or `z` for Zed. The editor opens the agent's project. If
the menu offers "Set up SSH" instead, choose it, which does the same as `agentctl ssh-config install`.

VS Code needs the [Remote - SSH](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-ssh)
extension. Zed needs its command, `zed`, on your `PATH`.

## Open it from the command line

Find the alias and the project:

```sh
agentctl ssh-info agent/NAME
```

Then open `Project` with `Alias`, here for an agent named `myapp`:

```sh
code --remote ssh-remote+agentctl-myapp /home/agent/code/myapp
```

```sh
zed ssh://agentctl-myapp/home/agent/code/myapp
```

## Open it in another editor

Any editor that connects to SSH hosts from `~/.ssh/config` works the same way: connect to the host `agentctl-NAME`
and open the folder `ssh-info` names as `Project`. A terminal reaches the agent with `ssh agentctl-NAME`, or with
`agentctl ssh agent/NAME` without the setup.

## When the folder isn't there

`Project` is the repository the agent's image clones when there is exactly one, and `/home/agent/code` otherwise. The
agent clones while it starts, so the folder can be missing for a minute after the agent turns `Ready`. If it stays
missing, the clone failed. List what is there:

```sh
agentctl ssh agent/NAME -- ls /home/agent/code
```

A private repository needs `GITHUB_TOKEN` in the agent's `.env`.
