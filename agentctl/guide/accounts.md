# GitHub accounts

An agent whose manifest has `accounts: [{type: github, name: altinn-studio}]` works on GitHub as one account: `gh` and
`git` over HTTPS work with GitHub, SSH remotes on github.com go over HTTPS, and commits carry the account's name and
email unless `environment` declares `GIT_USER_NAME` and `GIT_USER_EMAIL`. The agent sees only a placeholder
`GITHUB_TOKEN`; agentd keeps the token on the host under the name the manifest asks for, and puts it only into
requests to GitHub. An agent has at most one account: never write a second `accounts` entry, which the manifest check
rejects.

The name is the person's to choose: letters, digits and inner hyphens. Name it for what the token is for, by default
the repository the agent works in, such as `altinn-studio`. When `agentctl status --json` already lists a token under
`github` that the person wants this agent to use, ask for that name instead: several agents can share one token, and
one organization can have several tokens for different work.

Ask the person, in plain words, which of these they want:

- **The agent acts as a GitHub account made for agents**, such as `my-org-agent` or `alice-agent`, which works
  across organizations the way a person does. They sign in through the browser as that account.
- **The agent uses a token for one organization's repositories.** They create a fine-grained token in GitHub's form
  and pick the repositories there.

The person stores the token, never you. Ask them to run this in their own terminal, with the name the manifest asks for;
it asks which of the two, and opens a browser:

```sh
agentctl github login altinn-studio
```

For the browser sign-in it needs the GitHub CLI, `gh` (https://cli.github.com), and leaves their own `gh` login
alone. For the token form it asks for the organization, offering the owner of the repository it runs in; an
organization may have to approve the token before it reaches private repositories. agentctl shows which account the token acts as, and warns when it cannot see the repository or expires
soon, before it is stored. Never ask for the token or read it.

Ask first whether they have an account made for agents; if not, the token is the way. Never propose the person's
own account: what the agent does on GitHub should be told apart from what they do, and an account for agents reaches
only what it is given. agentctl does not check which account they sign in as; that is up to them. Which organizations and repositories
it reaches is up to that account or token, can change at any time, and you can't see it: don't write it in the
agent's instructions.

`agentctl status --json` lists the stored tokens under `github` (`agentctl guide status`). An agent whose name has
no token, or an expired one, runs without GitHub access and status names the login; once the person has run it, the
token works at once.

A manifest cannot have both an account and its own `GITHUB_TOKEN`. To move an agent from a token in `.env`, remove
the `GITHUB_TOKEN` secret from `agent.yaml` and the line from `.env`, add the account, have the person log in, and
apply again.

`agentctl github logout altinn-studio` ends the GitHub access of the agents asking for it at once and nothing else, but the token keeps working on
GitHub until it expires or the person revokes it there.
