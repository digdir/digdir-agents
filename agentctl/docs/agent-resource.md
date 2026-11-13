# Agent resource reference

The fields of an agent that clients and scripts read to find agents and open them, and the syntax of labels,
selectors and variant lists. `agentctl get agents -o json` prints the resource, `agentctl ssh-info -o json` an agent's
SSH access, and `agentctl get definitions -o json` the definitions a client can run.

Each of these is read by programs, so a newer `agentctl` may add fields. Ignore fields you don't know, and treat a
phase you don't know as `Unknown`.

## metadata

| Field | Set by | Meaning |
| --- | --- | --- |
| `name` | The manifest, or `apply --name` | The agent's name. A deleted agent's name can be used again. |
| `uid` | `agentd`, when it creates the agent | The agent's identity. It stays the same across applies, label changes and restarts, and a new agent with the same name gets a new one. |
| `labels` | The manifest, `apply --label` and `agentctl label` | What the agent is for. See [Labels](#labels). Absent when there are none. |
| `generation` | `agentd` | Goes up with each change to the spec. Labels don't change it. |
| `deletionTimestamp` | `agentd` | Set once deletion is requested. |

## status.phase

One word for what a person or a client can do with the agent now. `agentctl get agents` shows it as `STATUS`, and
`READY` is `True` only in `Ready`.

| Phase | Meaning |
| --- | --- |
| `Pending` | Applied, and no pass has reported on it yet. |
| `Provisioning` | A pass is building the image or preparing the sandbox. `status.progress` says which step. |
| `Waiting` | Nothing is provisioned until something outside the agent is there, such as a copy of a volume it mounts. The `Ready` condition says what. |
| `Starting` | The sandbox is starting, or a pass has not found the agent ready yet. |
| `Ready` | The agent can serve sessions and executions. |
| `Retrying` | A pass failed in a way another pass may not, and `agentd` tries again. |
| `Unresponsive` | The sandbox has stopped making progress. `agentd` keeps trying. |
| `Failed` | The manifest can't be provisioned as written. Applying a fixed one is the way out. |
| `Stopping` | A stop was requested, and the sandbox hasn't stopped yet. |
| `Restarting` | A restart was requested, and the sandbox hasn't stopped for it yet. |
| `Stopped` | Stopped by request, with its disk kept. |
| `Terminating` | Deletion was requested. |

`agentd` can add phases. A phase this build of `agentctl` doesn't know reads as `Unknown`, which means that the agent
can't be used.

The phase describes the agent as the last pass saw it. While `metadata.generation` is above
`status.observedGeneration`, a change is waiting for a pass. Each access type has its own condition as well, such as
`SshReady`, which `agentctl describe` lists.

## status.sandbox.projects

The checkouts the agent's image clones, as paths in the sandbox, such as `/home/agent/code/digdir-agents`. `agentd`
reads them from `/etc/agent-workspace.d/*.list` in the image once the sandbox is up. Each line there is
`OWNER/NAME [DESTINATION]`, and a checkout goes to `/home/agent/code/DESTINATION`, by default the repository's name.

The list says what the image declares, not what is on disk. The image clones at boot, so an agent can be `Ready`
before a checkout is there, and a checkout whose clone fails never appears. Absent when the image clones nothing.

## spec.access

How the agent can be reached. Each entry has a `type`.

| Type | Fields | Meaning |
| --- | --- | --- |
| `ssh` | None | SSH through `agentctl`. `agentctl ssh-info` describes it. |
| `vnc` | None | The agent's desktop over VNC. `agentctl vnc-info` describes it. |
| `web` | `name`, `port`, `path` | A web UI the image serves, such as T3 Code. An agent can declare several. |

A web view's fields:

| Field | Required | Rules |
| --- | --- | --- |
| `name` | Yes | 1 to 63 lowercase letters, digits and `-`, starting and ending with a letter or digit, such as `t3code`. Unique among the agent's views. |
| `port` | Yes | The port the view listens on in the sandbox, from 1 to 65535. |
| `path` | No | Where the view opens, starting with `/`. Letters, digits and `-._~!$&'()*+,;=:@/`, with any other character written as `%XX`. Defaults to `/`, and the resource leaves out a path of `/`. |

`agentctl describe` lists access as `ssh,web/t3code:3773`. Opening a view from `agentctl` isn't there yet.

## ssh-info

`agentctl ssh-info -o json` prints one object:

| Field | Meaning |
| --- | --- |
| `type` | Always `ssh`. |
| `agent` | The agent's name. |
| `agentId` | The agent's uid, which the key material belongs to. |
| `alias` | The OpenSSH `Host` alias in the generated configuration. |
| `user` | The user in the sandbox. |
| `identityFile` | The private key. |
| `knownHostsFile` | The `known_hosts` file with the agent's host key. |
| `configFile` | The generated OpenSSH client configuration. |
| `proxyCommand` | The `ProxyCommand` that reaches the agent through `agentctl`. |
| `workingDirectory` | The directory sessions open in, `/home/agent/code`. |
| `projectDirectory` | The directory an editor opens: the checkout when `status.sandbox.projects` has exactly one, and `workingDirectory` otherwise. |

`ssh-info` answers for an agent in any phase, so check `status.phase` before you connect.

## definitions

`agentctl get definitions -o json` prints an array with one object for each definition agentd lists for the current
directory. First come the manifests and variants anywhere in the Git repository that holds it or, outside a
repository, at `agent.yaml` and `agents/<name>/`, as `agentctl tui` and `apply` find them. Then come the manifests your
agents were applied from, and last the definitions agentd carries. A file that several paths lead to is listed once.
A client on the control API gets the same list for any folder from `definitions.v1.list { directory }`.

| Field | Meaning |
| --- | --- |
| `path` | The manifest file. For a definition agentd carries, agentd's copy of it. |
| `aliases` | Other paths that lead to the same file, such as through a symbolic link. Absent when there are none. |
| `reference` | For a definition agentd carries, the reference `agentctl run` takes, such as `bundled/agent-builder`. Absent for a file. |
| `name` | The `metadata.name` the definition gives its agent. Absent when it can't be read. |
| `error` | Why the definition can't be read. Absent when it can. |
| `slots` | The targets of its bind mounts without a `source`, which need a folder each when it is run. Absent when there are none. |

## Labels

A label is a key and a value, as
[in Kubernetes](https://kubernetes.io/docs/concepts/overview/working-with-objects/labels/):

- **Key:** an optional prefix and `/`, then a name. The name is at most 63 letters, digits, `-`, `_` and `.`, and
  starts and ends with a letter or digit. The prefix is a lowercase DNS subdomain of at most 253 characters.
- **Value:** empty, or written like a name.

An agent's labels come from two places:

| Source | Set with | An `apply` |
| --- | --- | --- |
| Declared | `metadata.labels` in the manifest or a variant | Replaces them with the ones the applied manifest declares. |
| Set from the host | `apply --label KEY=VALUE` or `agentctl label agent/NAME KEY=VALUE` | Keeps them. |

A label set from the host wins over a declared label with the same key. `agentctl label agent/NAME KEY-` removes the
key from both, and a manifest that still declares it gives it back on the next `apply`. No label change changes the
generation or restarts the agent.

Two keys are conventions that clients rely on:

| Label | Set by | Read by |
| --- | --- | --- |
| `client: NAME` | The person, in their own variant | A client that keeps a list of hosts, such as Gateway or T3 Code. It lists the agents labelled with its name. |
| `managed-by: NAME` | A tool, on the agents it creates | That tool, which lists only its own. Other clients leave these agents alone. |

## Selectors

`agentctl get agents -l SELECTOR` and `get agents -l SELECTOR --watch` list only the agents whose labels match, in
Kubernetes' syntax. Requirements are separated by commas, and an agent must meet all of them.

| Requirement | Matches an agent |
| --- | --- |
| `key=value`, `key==value` | with the label set to `value`. |
| `key!=value` | without the label, or with another value. |
| `key in (a,b)` | with the label set to `a` or `b`. |
| `key notin (a,b)` | without the label, or with a value other than `a` and `b`. |
| `key` | with the label set, to any value. |
| `!key` | without the label. |

An empty selector selects every agent. There is no OR across keys: run one `get` for each.

`-L KEY` adds a column with that label's value to the table, named after the key without its prefix. Repeat it, or
separate keys with commas.

## Variant lists

A variant merges these lists with the base's entry by entry, matching entries by their key:

| List | Key |
| --- | --- |
| `spec.access` | `type`, with `name` for `web` |
| `spec.accounts` | `type` |
| `spec.harnesses` | `type` |
| `spec.environment` | `name` |
| `spec.secrets` | `environment` |
| `spec.skills` | `name`, else the name of its `source` directory |
| `spec.sandbox.mounts` | `target` |

An entry in the variant:

- with a key the base has, merges into that entry, field by field, and keeps the fields it leaves out;
- with a key the base lacks, is added after the base's entries;
- with a field set to `null`, drops that field;
- with `$patch: delete`, removes the base's entry with its key. A key the base lacks is no error. The entry must name
  its key, so a `web` entry needs its `name`.

An empty list, `[]`, clears the base's list. Every other list, such as `spec.instructions` or a list of strings,
replaces the base's whole.
