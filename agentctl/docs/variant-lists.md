# Change an inherited list in a variant

A variant changes the lists it inherits entry by entry, so it holds only what it adds, changes or removes. This guide
does each of those. [agent-resource.md](agent-resource.md#variant-lists) lists which lists merge this way and the key
each one matches entries by.

The examples extend this base:

```yaml
# agent.yaml
apiVersion: agents.platform/v1alpha1
kind: Agent
metadata:
  name: worker
spec:
  harnesses:
    - type: claudeCode
      default: true
    - type: codex
      optional: true
  access:
    - type: ssh
    - type: web
      name: t3code
      port: 3773
```

## Add an entry

Write only the new entry. The base's entries stay, and the new one comes after them:

```yaml
# agent.desktop.yaml
apiVersion: agents.platform/v1alpha1
kind: AgentVariant
extends: agent.yaml
metadata:
  name: worker-desktop
spec:
  access:
    - type: vnc
```

The agent gets `ssh`, the `t3code` view and `vnc`. The examples below show only the variant's `spec`.

## Change a field of an entry

Write the entry's key and the fields to change. The fields you leave out keep the base's values:

```yaml
spec:
  access:
    - type: web
      name: t3code
      port: 3774
```

## Drop a field from an entry

Set the field to `null`:

```yaml
spec:
  harnesses:
    - type: codex
      optional: null
```

## Remove an entry

Write the entry's key and `$patch: delete`:

```yaml
spec:
  access:
    - type: web
      name: t3code
      $patch: delete
```

A `web` entry is known by its `name`, so `agentctl apply` refuses `$patch: delete` on one without it. Removing an
entry the base doesn't have is no error, so the variant keeps working if the base drops the entry later.

## Replace the whole list

`[]` clears the inherited list. To replace it with other entries, remove each inherited entry with `$patch: delete`
and add the new ones in the same list.

Lists without a key, such as `spec.instructions` and lists of strings such as `spec.network.deny`, have nothing to
merge by, so a variant that sets one replaces the base's whole.

## Check the result

`agentctl apply --variant desktop` applies the merged manifest, and `agentctl get agent/worker-desktop -o json` shows
the spec the agent got.
