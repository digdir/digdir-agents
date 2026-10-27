# GitHub accounts

An Agent works on GitHub with a token agentd stores under a name, separate from any `gh` login on the host. Its
manifest asks for the name:

```yaml
spec:
  accounts:
    - type: github
      name: altinn-studio
```

Names are yours to choose: letters, digits and inner hyphens. Several tokens can serve one organization, such as one
per task, each asked for by the manifests that need it.

`agentctl github login NAME` stores a token under that name. Without one it asks, offering the name the Agent here asks
for while it has no token, then this repository's name, then the folder's. It then asks how to get the token:

- Sign in through the browser as an account, such as an agent's own GitHub account that works across organizations as
  a person does. It runs the GitHub CLI's browser sign-in, `gh auth login --web`, against a configuration of its own
  that is deleted afterwards, so your own `gh` login is neither used nor changed; it needs `gh` from
  https://cli.github.com. The token reaches every repository that account can.
- Create a fine-grained token for one owner. It asks for the user or organization, offering the owner of this
  repository's GitHub remote, then opens GitHub's form with the owner, a 90-day lifetime and the permissions filled
  in: contents, issues, pull requests, Actions and workflow files read and write, and commit statuses read. You pick
  the repositories, which the form cannot be given, and paste the token; it is not shown.

Either way agentctl shows which account the token acts as and warns when it cannot see the repository or expires
within a week, before anything is stored, and says how a manifest asks for it. `--from-stdin` reads the token from
standard input instead, stores it under the name given or else this repository's, and warns of a pasted classic or
OAuth token reaching every repository of its account.

The Agent sees an inert `GITHUB_TOKEN`, which the network mediator replaces only in requests to `github.com`,
`api.github.com`, `gist.github.com` and `uploads.github.com`. Git uses it over HTTPS, SSH remotes on github.com go over
HTTPS, and commits carry the account's name and email unless the manifest declares `GIT_USER_NAME` and
`GIT_USER_EMAIL`. A manifest with an account cannot also declare its own `GITHUB_TOKEN`. A dedicated account, such as
a bot, keeps what Agents do apart from what you do.

`agentctl github list` shows each token's name, account, kind and expiry. An Agent whose name has no token, or an
expired one, runs without GitHub access, and `agentctl status` names the login. `agentctl github logout NAME` ends the
GitHub access of the Agents asking for it at once and nothing else; the token keeps working on GitHub until it expires
or you revoke it there.
