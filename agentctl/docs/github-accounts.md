# GitHub accounts

An Agent works on GitHub as one account per user or organization, separate from any `gh` login on the host. Its
manifest declares the owner:

```yaml
spec:
  accounts:
    - type: github
      owner: my-org
```

`agentctl github login` stores the owner's token, in one of two ways it asks you to choose between:

- Sign in through the browser as an account, such as an agent's own GitHub account that works across organizations as
  a person does. It runs the GitHub CLI's browser sign-in, `gh auth login --web`, against a configuration of its own
  that is deleted afterwards, so your own `gh` login is neither used nor changed; it needs `gh` from
  https://cli.github.com. The token is stored for `--owner`, or else for the account signed in as, and reaches every
  repository that account can.
- Create a fine-grained token for one owner. Run in a repository, it offers the owner of its GitHub remote, then opens
  GitHub's form with the owner, a 90-day lifetime and the permissions filled in: contents, issues, pull requests,
  Actions and workflow files read and write, and commit statuses read. You pick the repositories, which the form
  cannot be given, and paste the token; it is not shown.

Either way agentctl shows which account the token acts as and warns when it cannot see the repository or expires
within a week, before anything is stored, and says which owner the manifest names. `--from-stdin` reads the token from
standard input instead, and warns of a pasted classic or OAuth token reaching every repository of its account.

The Agent sees an inert `GITHUB_TOKEN`, which the network mediator replaces only in requests to `github.com`,
`api.github.com`, `gist.github.com` and `uploads.github.com`. Git uses it over HTTPS, SSH remotes on github.com go over
HTTPS, and commits carry the account's name and email unless the manifest declares `GIT_USER_NAME` and
`GIT_USER_EMAIL`. A manifest with an account cannot also declare its own `GITHUB_TOKEN`. A dedicated account, such as
a bot, keeps what Agents do apart from what you do.

`agentctl github list` shows each owner's account, kind and expiry. An Agent whose owner has no token, or an expired
one, runs without GitHub access, and `agentctl status` names the login. `agentctl github logout OWNER` ends Agents'
GitHub access at once and nothing else; the token keeps working on GitHub until it expires or you revoke it there.
