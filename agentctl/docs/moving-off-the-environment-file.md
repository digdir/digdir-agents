# Moving off the environment file

A later release of agentctl stops reading values from an agent's environment file, the `.env` beside its manifest or
the file `--env-file` named. The file sat in or near a checkout, where it was easy to commit or to mount into an
agent, and every agent kept its own copy of the same keys. Each kind of value now has a place of its own.

`agentctl describe agent/NAME` shows the agents that still read the file: their `EnvironmentFile` condition names the
variables to move. Once you have moved them, apply the agent again, and delete the file when no agent reads it. Until
then, agentctl mounts no folder that holds the file into an agent, such as the checkout it is in.

`agentctl update` moves a checkout, or a folder of personal agents, off the file in one go. At a terminal it offers to
store the file's GitHub token as the login `default` and each secret under a name made from its variable, deletes the
file, and has the agent builder change the manifests to take the values from there. It shows plain values, which the
builder writes into the manifests, and names any other keys in the file, which are deleted with it. You apply your
agents again once the manifests take their values from agentctl. After you pull manifests that someone else moved this
way, `agentctl update` stores what they take from the variable each login or secret is named after, and deletes the
file. When it cannot store a value for you, such as one that differs between two files, it says how to store it
yourself.

The agent builder is also given your folder of personal agents, so when one of those still reads such a file,
`agentctl update` asks you to run it on that folder first. Any other command that would mount a folder holding the file
refuses and points you to `agentctl update`. Once no manifest beside the file and no agent on this machine reads it,
those commands name what the manifests take from agentctl, and at a terminal offer to delete the file. The sections
below say where each kind of value goes, for moving a manifest by hand.

## A GitHub token, and Git's name and email

Sign in to GitHub under a name, and give the agent that account. It gets a `GITHUB_TOKEN` for GitHub's hosts, and Git
commits as the account, so it no longer needs `GIT_USER_NAME` and `GIT_USER_EMAIL`:

```sh
agentctl github login default
```

```yaml
spec:
  accounts:
    - type: github
      login: default
  # Remove the GITHUB_TOKEN secret, and GIT_USER_NAME and GIT_USER_EMAIL from environment.
```

## An API key, or another secret

Store it under a name, with the hosts its value may reach, and ask for it by that name:

```sh
agentctl secret set aivar --hosts api.aivar.no
```

```yaml
spec:
  secrets:
    - name: aivar
      environment: AIVAR_API_KEY
```

Every agent that names it shares the stored value, and `agentctl secret set` replaces it for all of them at once.
`allowedHosts` on the entry narrows the secret's hosts for this agent.

## A plain value

Give a value that is the same for everyone in the manifest itself. The agent sees it as it is, so never give a secret
this way:

```yaml
spec:
  environment:
    - name: DOTNET_CLI_TELEMETRY_OPTOUT
      value: "1"
```

A value of your own goes in a variant of your own beside the manifest, such as `agent.mine.yaml`, which you keep out of
Git. A variant adds its entries to the base's list, and an entry with the `name` of one there changes that one, so it
names only what it adds or changes:

```yaml
apiVersion: agents.platform/v1alpha1
kind: AgentVariant
extends: agent.yaml
spec:
  environment:
    - name: EDITOR
      value: vim
```

## Inside an agent

An agent that runs agents of its own stores their secrets from its own variables. These hold placeholders, which the
host it runs on replaces with the real values in requests to their hosts, so the agents it runs reach them too:

```sh
printf '%s' "$GITHUB_TOKEN" | agentctl github login default --from-stdin
printf '%s' "$AIVAR_API_KEY" | agentctl secret set aivar --hosts api.aivar.no --from-stdin
```
