# The manifest: agent.yaml

An agent is declared by `agent.yaml` in its own folder, such as `agents/dev/` at the root of a repository.
`agentctl apply -f agents/dev/agent.yaml` creates or updates it; from inside the folder, `agentctl apply` alone does.
Relative paths in a manifest (the build context, instructions, mount sources) resolve against the manifest's
directory, and the agent belongs to that directory: commands such as `attach` find it from anywhere in the
repository.

Values come from `.env` beside the manifest, or from the file `agentctl apply --env-file` names; agentd remembers that
file for later applies. One `NAME=value` per line. `environment` copies the named values into the agent as plain
environment variables. `secrets` never enter the agent: it sees a placeholder, and the host puts the real value into
requests to the hosts listed for it. Every name either lists must be in the file unless marked `optional`. Keep the
file out of Git.

Work on the person's checkout by bind-mounting it, as below, or leave `mounts` out and let the agent clone its own
copy in `~/code`, as its instructions list. agentd refuses a bind mount that contains an agent's `.env`, so an agent
that mounts the checkout it is defined in takes its values from a file outside it: `agentctl apply -f
agents/dev/agent.yaml --env-file ~/.agentctl-dev.env`.

A variant, `agent.<variant>.yaml` in the same folder, changes only what differs from `agent.yaml`. Mappings merge,
lists and values replace, and `null` removes. It is a separate agent with its own name; apply it with
`agentctl apply -f agents/dev/agent.small.yaml`:

```yaml
apiVersion: agents.platform/v1alpha1
kind: AgentVariant
extends: agent.yaml
metadata:
  name: dev-small
spec:
  sandbox:
    resources:
      cpu: "2"
      memory: "4Gi"
```

A complete manifest, with the fields a person is likely to set:

