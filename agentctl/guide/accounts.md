# GitHub accounts

An agent whose manifest has `accounts: [{type: github, owner: my-org}]` works on GitHub as one account for that user
or organization: `gh` and `git` over HTTPS work with the owner's repositories, SSH remotes on github.com go over
HTTPS, and commits carry the account's name and email unless `environment` declares `GIT_USER_NAME` and
`GIT_USER_EMAIL`. The agent sees only a placeholder `GITHUB_TOKEN`; agentd keeps the token on the host, one per
owner, and puts it only into requests to GitHub.

The person stores the token, never you. Ask them to run this in their own terminal, from the repository so it offers
its owner, or with `--owner my-org`:

```sh
agentctl github login
```

It opens GitHub's form for a fine-grained token with the owner, a 90-day lifetime and the permissions filled in.
They pick the repositories the agent needs there; the form cannot be given them. An organization may have to approve
the token before it reaches private repositories. agentctl shows which account the token acts as, and warns when it
cannot see the repository or expires soon, before it is stored. Never ask for the token or read it.

Suggest a dedicated account, such as a bot, rather than the person's own: what the agent does on GitHub is then
told apart from what they do, and the account reaches only what it is given.

`agentctl status --json` lists the stored tokens under `github` (`agentctl guide status`). An agent whose owner has
no token, or an expired one, runs without GitHub access and status names the login; once the person has run it, the
token works at once.

A manifest cannot have both an account and its own `GITHUB_TOKEN`. To move an agent from a token in `.env`, remove
the `GITHUB_TOKEN` secret from `agent.yaml` and the line from `.env`, add the account, have the person log in, and
apply again.

`agentctl github logout my-org` ends agents' GitHub access at once and nothing else, but the token keeps working on
GitHub until it expires or the person revokes it there.
