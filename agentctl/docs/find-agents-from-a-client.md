# Find the agents meant for your client

A client is an application on the host that a person works in an agent through, such as an editor, JetBrains
Gateway, T3 Code or a tray. This guide is for people writing one: it finds the agents meant for the client, decides
whether one can be used, and finds what to open in it. Every step reads `agentctl` with `-o json`, so the client
needs no Rust code of its own. [agent-resource.md](agent-resource.md) describes each field, and
[the client discovery note](design/client-discovery.md) explains why it works this way.

## Pick a selector

Decide which agents the client touches, and touch no others:

- **A client that shows agents a person chose for it**, such as a list of hosts, selects `client=NAME`. A person sets
  that label in their own variant, as [Label agents](label-agents.md#mark-the-client-you-open-an-agent-in) shows.
- **A client that creates agents itself** labels each one with `managed-by=NAME` when it creates it, and selects
  `managed-by=NAME`:

  ```sh
  agentctl apply -f "$REPOSITORY/agent.yaml" --name "$AGENT" --label managed-by=NAME --wait
  ```

Use the client's own name as `NAME`, such as `t3code`, and the same name in both labels.

## Follow the agents

Start one watch and read it line by line:

```sh
agentctl get agents -l client=NAME --watch -o json
```

Each line is a JSON array of every agent that matches now. Replace the client's list with each line rather than
applying changes, since an agent that stops matching is simply absent from the next one. A line is printed only when
something changed. The watch keeps running and reconnects by itself when `agentd` restarts. It exits with an error
when `agentd` comes back as a build that doesn't match this `agentctl`, such as after an update, so start it again
then.

## Keep an agent by its uid

When the client remembers an agent, such as an entry in a host list, store `metadata.uid` beside `metadata.name`. An
agent with the same name and another uid is a new agent: the one you knew was deleted. Drop or refresh what you kept
for the old one.

## Decide whether an agent can be used

Read `status.phase`:

- Offer to open an agent only in `Ready`.
- Show the other phases as they are, such as `Provisioning` or `Stopped`, so the person knows what to wait for.
- Treat a phase you don't know as not usable, since `agentd` can add phases.

To open it over SSH, also check that the `SshReady` condition in `status.conditions` is `True`.

## Open the project

```sh
agentctl ssh-info agent/NAME -o json
```

Connect with `alias` and `configFile`, or with `identityFile`, `knownHostsFile`, `user` and `proxyCommand`, and open
`projectDirectory` as the folder. When the image clones more than one repository, `projectDirectory` is
`/home/agent/code`, and `status.sandbox.projects` on the agent lists each checkout, so the client can offer a choice.

The image clones at boot, so a checkout may not be there yet when the agent turns `Ready`, and won't appear if the
clone fails. Fall back to `workingDirectory` when the folder is missing.

## List the agent's web views

Each `spec.access` entry with `type: web` is a web UI the agent serves, with a `name`, a `port` in the sandbox and a
`path`, which is `/` when absent. A client can list them by name. `agentctl` doesn't open them yet, so a client that
opens one forwards the port itself for now.

## Offer the definitions on disk

To let a person create an agent, list the definitions for their folder:

```sh
cd "$FOLDER" && agentctl get definitions -o json
```

Each entry is a manifest, a variant or a definition agentd carries. Offer the ones without `error`, and show `name` as
the agent it creates. `agentctl apply -f PATH` creates or updates the agent from a file, and `agentctl run` starts one
from a `reference` or a `path`, given a folder for each target in `slots`.
