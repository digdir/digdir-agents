# How clients find agents

Status: proposal for [#385](https://github.com/digdir/digdir-agents/issues/385).

A client is an application on the host that a person works in an agent through, or that creates and drives agents
itself: an editor over SSH, JetBrains Gateway, the Claude app, T3 Code, the TUI, a tray or another GUI on the Control
API. Before a client can open an agent, it has to find it. Which agents exist, which of them are meant for this
client, can one be used now, where is its project, and what does it serve? `agentctl` answers part of that today. Each
client works out the rest itself, and the answers drift apart.

This note proposes what the agent resource and the Control API add so that every client reads the same answers. How a
client then opens an agent, `agentctl connect` and the client registry, is
[#386](https://github.com/digdir/digdir-agents/issues/386) and builds on this.

## What clients read today

`agentctl get agents --watch -o json` prints every agent's spec and status each time one changes. The TUI and T3 Code
watch it. `agentctl ssh-info -o json` describes one agent's SSH access, and `agentctl vnc-info` its desktop.

The rest, clients work out themselves:

| What | Who works it out | What goes wrong |
| --- | --- | --- |
| Whether an agent can be used now | The TUI, and T3 Code with a copy of the TUI's rules | The answer combines deletion, run state, conditions, failure and progress into ten states. There are two copies already, and a tray or GUI would make a third. |
| Which agents a client should touch | Nobody | The only signal is `access: ssh`, which says that an agent can be reached, not whom it is for. A T3 Code server on a host started itself in every agent with SSH, including one a person had open in Rider. |
| Whether an agent is the one the client knew | Nobody | A resource carries only its name, and a deleted agent's name can come back as a new agent ([#301](https://github.com/digdir/digdir-agents/issues/301)). Gateway's and the Claude app's host lists and T3 Code's environments keep a reference longer than an agent may live. |
| The project to open | Nobody | `ssh-info` always says `/home/agent/code`. The repository the image clones is below it, so every editor opens beside the project rather than in it. |
| The web UIs an agent serves | A person, from the image's docs | The image sets the port, and nothing the host can read records it ([#387](https://github.com/digdir/digdir-agents/issues/387)). |
| The agent definitions on disk | The TUI, `apply` with [#309](https://github.com/digdir/digdir-agents/pull/309), and T3 Code | Three walks of the filesystem, each with its own rules for which files are variants. |

## Proposal

### The agent's identity

The resource gets `metadata.uid`, the `AgentId` that `agentd` already keys everything on. Only `agentd` sets it. A
client that keeps a reference keeps the uid beside the name and treats an agent with the same name and another uid as
a different agent. T3 Code can derive its stable environment id from it
([#166](https://github.com/digdir/digdir-agents/issues/166)).

This is the first step of #301. Accepting the uid as a precondition on writes, which closes the races #301 lists, can
follow without changing what clients read.

### One phase, computed by `agentd`

`status.phase` says in one word what a person or a client can do with the agent now:

| Phase | Meaning |
| --- | --- |
| `Pending` | Applied, and `agentd` has not started on it. |
| `Provisioning` | `agentd` is building the image or preparing the sandbox. `status.progress` says which step. |
| `Starting` | The sandbox is booting, or the agent is not ready yet. |
| `Ready` | The agent can be used. Each access type still has its own condition, such as `SshReady`. |
| `Retrying` | A pass failed, and `agentd` tries again. |
| `Unresponsive` | The sandbox has stopped answering. |
| `Failed` | The manifest cannot be provisioned as written. Applying a fixed one is the way out. |
| `Stopping`, `Stopped` | Stopped by request, with the disk kept. |
| `Terminating` | Deletion has been requested. |

`agentd` derives it from the fields the TUI combines today, in one place, and the TUI, `agentctl status`
([#311](https://github.com/digdir/digdir-agents/pull/311)), T3 Code and a GUI read it.

Kubernetes moved away from a phase in favour of conditions, because one word hides detail. Here the conditions stay
the detail. The phase is there because every client needs the same summary, and each one that computes it gets it
slightly wrong.

### The project

`agentd` reads which repositories the image clones, from `/etc/agent-workspace.d/*.list` as the
[base images](../../../images/agentctl-base/README.md) do, once the sandbox is up. The agent's status lists them as
paths in the sandbox. `ssh-info` gets `projectDirectory`: the repository's checkout when the image clones exactly
one, and the working directory otherwise. Editors and `connect` open that, and a client can offer the list when there
are several.

### Labels

Gateway, the Claude app and T3 Code each keep their own list of hosts, and something has to decide which agents go
in which list. Today the answer is every agent with SSH. That puts a person's Rider agent into T3 Code, and an
orchestrator's workers into Gateway.

The resource gets `metadata.labels`, keys with string values, as
[in Kubernetes](https://kubernetes.io/docs/concepts/overview/working-with-objects/labels/). They say what an agent is
for without changing what it is, so changing one changes no generation and restarts nothing.

They are set from the host:

- In a manifest or variant, under `metadata.labels`. This is where a person's choice of view goes.
- With `agentctl apply --label KEY=VALUE`. An orchestrator labels the agents it creates from a repository's own
  `agent.yaml` without writing a variant for each repository.
- With `agentctl label agent/NAME KEY=VALUE`, and `KEY-` to remove one, for an agent that is already applied.

`apply` replaces the labels its manifest declares and keeps the ones set with `--label` or `agentctl label`, which
only `agentctl label KEY-` removes. A person who edits their variant gets what it says, and an orchestrator's label
survives a person applying the same definition by hand.

The sandbox cannot set labels, since the Control API is local to the host. A nested `agentd` inside an orchestrator's
sandbox keeps its own agents and their labels.

`get` and `get --watch` take `-l`/`--selector`, with Kubernetes' syntax for equality and sets. Two keys are
conventions that clients rely on:

| Label | Set by | Read by |
| --- | --- | --- |
| `client: NAME` | The person, in their variant | A client that keeps a host list. It lists the agents labelled with its name, and `agentd`'s subscription writes and removes their entries as they come and go. |
| `managed-by: NAME` | An orchestrator, on the agents it creates | That orchestrator, which lists only its own. Other clients leave these agents alone. |

A client touches only the agents its selector matches. That is the rule #385 states, now with something to check
against. An agent with neither label still opens in any client a person picks for it, as from the TUI's open menu.

```sh
# T3 Code creates a worker from the repository's own definition, and watches only its own workers
agentctl apply -f agents/notifications/agent.yaml --name notifications-1 --label managed-by=t3code
agentctl get agents -l managed-by=t3code --watch -o json

# The agents a person created themselves, with the view each one is for
agentctl get agents -l '!managed-by' -L client
```

### Web views

An agent declares the web UIs it serves in the manifest, under `access`, as #387 proposes:

```yaml
access:
  - type: web
    name: T3 Code
    port: 3773
    path: /
```

The manifest is read on the host, so a sandbox cannot add views. The resource carries them, so a client lists an
agent's views without knowing its image. Reaching a view, with a forward and a sign-in link, stays in #387.

Two things about `access` change with it. Today an access type takes no fields, and a view needs some. And
[#395](https://github.com/digdir/digdir-agents/issues/395) merges `access` in variants by `type`, which allows one
entry per type. Views need `type` and `name` together as the key.

### Agent definitions on disk

`agentctl definitions [DIR] -o json` lists the agent definitions under a directory. For each manifest and variant it
gives the path, the variant, the agent name it declares, the repository it belongs to and the applied agent that came
from it, if one did. It uses the lookup the TUI has today and #309 gives `apply`, moved into the `agent` library, so
the TUI, `apply`, T3 Code and a GUI find the same definitions. It is not a `get`, because the files are not resources
in `agentd`.

## Where a client's choice lives

**A view** shows one agent, usually for one repository: VS Code, Zed, Gateway, the Claude app. Choosing one is a
person's choice for that repository, so it goes in their own variant beside the repository's manifest, which Git
ignores. Any tool can write it: a person by hand, `agentctl setup` ([#359](https://github.com/digdir/digdir-agents/pull/359)),
the agent builder ([#368](https://github.com/digdir/digdir-agents/pull/368)) or a GUI. With #395 the variant holds
only what the view adds:

```yaml
apiVersion: agents.platform/v1alpha1
kind: AgentVariant
extends: agent.yaml
metadata:
  name: notifications
  labels:
    client: jetbrains
spec:
  access:
    - type: ssh
  sandbox:
    resources:
      memory: "16Gi"
```

**An orchestrator** creates and drives many agents, often from several repositories, such as T3 Code delegating to
the sandboxes it creates. To it, a repository's definition is a template, so its choice must not need a variant per
repository. It applies the repository's own `agent.yaml` with `--label managed-by=NAME` and prepares the agent over
the SSH access it already uses. Which definitions it may create agents from is an open question in #385.

## Not covered

- Opening an agent in a client: `connect` and the client registry (#386), and each client's own wiring
  ([#370](https://github.com/digdir/digdir-agents/issues/370), [#371](https://github.com/digdir/digdir-agents/issues/371),
  #166).
- Forwards that outlive the client that opened them ([#273](https://github.com/digdir/digdir-agents/issues/273)).
- Manifest values without a `.env` file ([#346](https://github.com/digdir/digdir-agents/issues/346)).
- Agents on peers ([#275](https://github.com/digdir/digdir-agents/issues/275)). Labels and the phase are part of the
  resource, so they travel with it, and a selector can work across peers later.

## Open questions

- **Label keys.** Plain keys such as `client` and `managed-by`, or keys with a prefix, as Kubernetes uses
  `app.kubernetes.io/managed-by`, so a person's own labels never mean something to a client by accident.
- **Selectors on `stop` and `delete`.** They make cleaning up after an orchestrator one command, and a wrong selector
  stops or deletes many agents at once.
- **The phase in `agentctl status`.** Whether #311 prints `status.phase` as it is, or keeps a summary of its own.
