# The manifest: agent.yaml

An agent is declared by `agent.yaml` in its own folder, such as `agents/dev/` at the root of a repository, or of
the folder the person works in when it is not a repository.
`agentctl apply -f agents/dev/agent.yaml` creates or updates it; from inside the folder, `agentctl apply` alone does.
Relative paths in a manifest (the build context, instructions, mount sources) resolve against the manifest's
directory, and the agent belongs to that directory: commands such as `attach` find it from anywhere in the
repository or that folder. A path starting with `~/` is in the person's home directory. Mark an instruction file or
skill that is not committed with the agent, such as one under `~/`, `optional: true`, so that the agent still starts
on a machine without it. Leave a file committed in the same repository required, so a wrong path still fails.

Values come from `.env` beside the manifest, or from the file `agentctl apply --env-file` names; agentd remembers that
file for later applies. One `NAME=value` per line. `environment` copies the named values into the agent as plain
environment variables. `secrets` never enter the agent: it sees a placeholder, and the host puts the real value into
requests to the hosts listed for it. Every name either lists must be in the file, except a secret marked `optional:
true`. Keep the file out of Git.

Work on the person's checkout by bind-mounting it, as below, or leave `mounts` out and let the agent clone its own
copy in `~/code`, as its instructions list. agentd refuses a bind mount with a file named `.env` anywhere in it, in
any case, hidden or ignored by Git included, so an agent that mounts the checkout it is defined in takes its values
from a file outside it: `agentctl apply -f agents/dev/agent.yaml --env-file ~/.agentctl-dev.env`. Before proposing a
mount, look for a `.env` in the folder; if there is one, propose a clone instead. Never remove or move the person's
file.

Pi has no login on the host, so `login_stored` is `null` for it in `agentctl status --json`. It reads its providers
and models from `~/.pi/agent/models.json`: put that file in a `home` folder beside the manifest, such as
`agents/dev/home/.pi/agent/models.json` with `home: {source: home}` under `spec`, which agentd copies into
`/home/agent` on every pass. Write the provider's key there as `"apiKey": "$PROVIDER_API_KEY"`, declare
`PROVIDER_API_KEY` as a secret allowed only to the provider's host, and set the harness's `defaults.model` to
`<provider>/<model id>`. Without the key Pi starts, but its prompts time out.

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

