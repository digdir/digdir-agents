# Use an agent from T3 Code

Run [T3 Code](https://github.com/pingdotgg/t3code) threads inside an agent. T3 Code adds the agent as a remote
environment over SSH and runs its server there, so each thread's Claude Code works on the agent's clone, with the
agent's tools and its mediated logins.

The steps were tried with T3 Code 0.1.8 on Linux, against the agent from [the tutorial](first-agent.md). The agent
needs `access` with `type: ssh` and a harness, such as `claudeCode`, in its manifest, and must be `Ready`.

## Set up SSH once

```sh
agentctl ssh-config install
```

T3 Code finds SSH hosts in `~/.ssh/config` and the file this includes, where each agent is `agentctl-NAME`.

## Add the agent as an environment

1. The first time T3 Code starts, it sets up its local environment. Choose Start. To use T3 Code only with agents,
   turn "Local environment" off later, in Settings, under Environments.
2. Open Settings, then Environments, and choose "Add environment".
3. Choose SSH. Under "SSH host or alias", choose the agent's alias, such as
   `agentctl-profile-backend-developer-nested-build`. T3 Code fills in `agent` as the username and `22` as the port.
4. Choose "Add environment".

T3 Code installs its server in the agent and starts it. It takes about 440 MB of the agent's home and 400 MB of its
memory while it runs.

## Open the repository

1. Add a project and choose the agent's environment.
2. Choose "Local folder", and enter the folder `agentctl ssh-info agent/NAME` names as `Project`, relative to the
   agent's home, such as `code/altinn-profile`.
3. Start a thread.

The thread runs the Claude Code in the agent's image, with the login `agentctl` gives the agent, so it needs no login
of its own. Asked to run `hostname` and `pwd`, it answered with the agent's name and
`/home/agent/code/altinn-profile` in under 7 seconds. The thread's panel shows the branch and the changes against
`origin`, and "Open in VS Code" opens the same folder over SSH.

## When adding the environment fails

- **`microsandbox install operation in progress until …; retry after it completes`**: `agentd` was busy setting up
  the agent's sandbox runtime. Wait until the time it names and add the environment again.
- **`Connection timed out during banner exchange`**: the agent answered SSH too slowly, as on a machine short of CPU
  while an image builds. Add the environment again.
- **The alias isn't in the list**: SSH isn't set up, or the agent has no `ssh` access. `agentctl ssh-info agent/NAME`
  says which.
