# GitHub accounts

An agent works on GitHub with a token that agentd keeps under a name, separate from any `gh` login on the host. The
agent's manifest asks for the name:

```yaml
spec:
  accounts:
    - type: github
      name: altinn-studio
```

The name is yours to choose: letters, digits and inner hyphens. Several tokens can serve one organization, such as one
per task, each asked for by the manifests that need it. An agent has at most one account of each type, so it has one
GitHub token and one Git identity.

## Keeping a token

`agentctl github login NAME` keeps a token under that name. Without a name it uses this repository's. At a terminal it
asks which user or organization the token is for, offering the owner of this repository's GitHub remote. It then opens
GitHub's form for a fine-grained token, with the owner, a lifetime and the permissions agents need filled in, as
[`PERMISSIONS`](../src/github/mod.rs) lists them. You pick the repositories, which the form can't be given, and paste
the token. It isn't shown.

`--from-stdin` reads the token from standard input instead. That also takes a classic token or your own GitHub CLI
login:

```sh
gh auth token | agentctl github login NAME --from-stdin
```

Think twice before doing that. A classic token or a `gh` login lets agents act as you in every repository and
organization you can reach, while a fine-grained token reaches only the repositories you picked. A dedicated account,
such as a bot, keeps what agents do apart from what you do.

agentd asks GitHub which account the token acts as and when it expires, and refuses a token GitHub rejects, one that
has expired, and kinds that stop working on their own, such as GitHub App tokens that need refreshing. Once the token
is kept, agentctl shows the account and warns when the token can't see this repository, expires within a week,
reaches beyond the repositories it was created for, or lacks scopes `gh` needs.

## What the agent gets

The agent sees an inert `GITHUB_TOKEN`, which the network mediator replaces with the real token only in requests to
GitHub's hosts. The token itself never enters the sandbox. Git uses it over HTTPS, and SSH remotes on github.com go
over HTTPS, since the sandbox has no SSH key. Commits carry the account's name and email unless the manifest sets
`GIT_USER_NAME` and `GIT_USER_EMAIL`. A manifest with a GitHub account can't also declare its own `GITHUB_TOKEN`.

A login or logout reaches the agents asking for that name at once, without restarting them.

## Listing and forgetting

`agentctl github list` shows each token's name, account, kind and expiry, and the names agents ask for that have no
token. An agent whose token is missing runs without GitHub access until you keep one.

`agentctl github logout NAME` ends the GitHub access of the agents asking for it, and nothing else. The token keeps
working on GitHub until it expires or you revoke it there, and the command says where.
