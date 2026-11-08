# Label agents and list them by label

Labels say what an agent is for, such as the client you open it in or the tool that created it. Clients and scripts
then list only the agents meant for them. This guide sets labels and selects agents by them.
[agent-resource.md](agent-resource.md#labels) has the rules for keys, values and selectors.

## Mark the client you open an agent in

A client that keeps a list of hosts, such as JetBrains Gateway or T3 Code, lists the agents whose `client` label
names it. Choosing a client for a repository's agent is your choice, so put it in your own variant beside the
repository's `agent.yaml`, such as `agent.mine.yaml`, rather than in the shared manifest:

```yaml
apiVersion: agents.platform/v1alpha1
kind: AgentVariant
extends: agent.yaml
metadata:
  name: notifications
  labels:
    client: jetbrains
```

Apply it:

```sh
agentctl apply --variant mine
```

The variant's labels merge with those the base declares.

## Label an agent that is already running

```sh
agentctl label agent/notifications client=jetbrains team=core
```

`agentctl label agent notifications client=jetbrains` works too. The label takes effect at once, and the agent keeps
running.

## Remove a label

```sh
agentctl label agent/notifications client-
```

If the manifest declares the label, the next `apply` brings it back. Remove it from the manifest as well to keep it
off.

## Go back to the manifest's value

A label you set with `agentctl label` or `apply --label` wins over the manifest's value for the same key, and an
`apply` keeps it. To use the manifest's value again, remove the key and apply:

```sh
agentctl label agent/notifications client-
agentctl apply --variant mine
```

## Label the agents a tool creates

A tool that creates agents from a repository's own `agent.yaml` labels them with `managed-by` when it creates them,
without writing a variant:

```sh
agentctl apply -f agents/worker/agent.yaml --name worker-1 --label managed-by=my-tool --wait
```

A later `apply` of the same agent keeps the label without repeating `--label`. Clients that keep host lists leave
agents with `managed-by` alone, so leave it off an agent that is meant for a person to open.

## List agents by label

```sh
# The agents for one client
agentctl get agents -l client=jetbrains

# The agents for either of two clients
agentctl get agents -l 'client in (jetbrains,t3code)'

# The agents a person created, with each one's client as a column
agentctl get agents -l '!managed-by' -L client

# One tool's agents for one team: every requirement must match
agentctl get agents -l managed-by=my-tool,team=core
```

Quote a selector that holds `!`, spaces or parentheses, so the shell passes it as it is. A selector can't combine
different keys with OR. Run one `get` for each instead.

`agentctl describe agent/notifications` shows all of an agent's labels.

## Follow the agents that match

```sh
agentctl get agents -l client=t3code --watch -o json
```

Each time the matching agents change, `get --watch -o json` prints one line holding all of them as a JSON array. An
agent that is relabelled out of the selector drops out of the next line, and the line is `[]` when none match.
