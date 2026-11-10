# Give an agent GitHub access

Let an agent clone, push and open pull requests as a GitHub account, without the token ever entering the agent. You
store a login under a name on your machine, and manifests ask for it by that name.

## What a login gives the agent

An agent whose manifest has

```yaml
accounts:
  - type: github
    login: default
```

gets, while the login `default` is stored:

- `GITHUB_TOKEN`, holding a placeholder that `agentctl` swaps for the real token in requests to GitHub's hosts. `git`
  and `gh` in the agent use it.
- Git's `user.name` and `user.email`, set to the token's account, so the agent commits as it.
- The clone at boot. The image's workspace list names the repositories, and the agent clones them with `gh` as it
  starts. `gh` needs the token even for a public repository.

The agent builder writes manifests that ask for `default`. A repository's own definitions may ask for a name of their
own, such as `altinn-studio`, and `agentctl github list` shows the names agents ask for that you haven't stored.

## Store a login

```sh
agentctl github login default
```

It asks whose repositories the token will reach:

```text
? Which GitHub user or organization's repositories will the token reach?
  Owner: Altinn
```

Then it prints a link to GitHub's form for a fine-grained token, filled in with a name, the owner, a 90-day lifetime
and these repository permissions:

| Permission | Access |
| --- | --- |
| Contents, Issues, Pull requests, Actions, Workflows | Read and write |
| Checks, Commit statuses | Read |

1. Open the link signed in as the account the agents should act as. To keep your own account's session, use a private
   window, or sign in as a bot account there.
2. Under "Repository access", choose "Only select repositories" and pick the repositories the agents need. The link
   can't fill those in.
3. Generate the token and paste it at the prompt. It isn't shown.

`agentctl` checks the token with GitHub and says who the agents act as:

```text
Checking the token with GitHub...

  Login:    default
  Acts as:  your-account (Your Name <12345678+your-account@users.noreply.github.com>)
  Kind:     fine-grained
  Expires:  2027-02-07
Agents asking for the login default now use GitHub as your-account.
Agents use it with `accounts: [{type: github, login: default}]` in their manifest.
```

Running agents that ask for the login use it at once. One that started without it has no clone yet:
`agentctl restart agent/NAME` clones what is missing as it starts.

### Choose the owner

A fine-grained token reaches only the repositories of the owner you choose, so choose the owner of the repositories
the agent works on:

- **Your own repositories:** your account.
- **An organisation's repositories, such as Altinn's:** the organisation. The organisation must allow fine-grained
  tokens, and may require an owner to approve each one before it works. The account still needs write access to the
  repositories, through a team or as a collaborator.

A token owned by your account reads public repositories of an organisation, so the clone works, but it never pushes
to them, even when your account is in a team with write access.

### Use another kind of token

`--from-stdin` reads a token instead of opening the form, such as a classic token or your GitHub CLI login's:

```sh
gh auth token | agentctl github login default --from-stdin
```

A classic token or a `gh` login reaches everything its account can, in every organisation, so agents using it act as
you everywhere you can.

## Check it from the agent

```sh
agentctl ssh agent/NAME -- gh api user --jq .login
agentctl ssh agent/NAME -- gh api repos/OWNER/REPO --jq .permissions
agentctl ssh agent/NAME -- git config user.email
```

The first prints the account the agent acts as. The second prints what it may do in a repository, such as:

```json
{"admin":false,"maintain":false,"pull":true,"push":false,"triage":false}
```

### When the agent cannot push

`"push": false`, or `git push` failing with `403`, means one of these:

- The token's owner isn't the repository's owner. Store the login again with the organisation as owner.
- The repository isn't among the token's selected repositories. Edit the token on GitHub, or make a new one.
- The organisation hasn't approved the token yet. Its owners see it under the organisation's settings, in "Personal
  access tokens".
- The account has no write access to the repository.

## Keep several logins

Store each under a name of its own, such as one for work on Altinn and one for your own repositories, and name the
login in each manifest:

```sh
agentctl github login altinn
agentctl github login private
agentctl github list
```

```text
LOGIN    ACCOUNT       KIND          EXPIRES
altinn   your-account  fine-grained  2027-02-07
private  your-account  fine-grained  2027-02-07
```

Storing a login under a name it already has replaces it for every agent that uses it, which is also how you renew one
before it expires.

## Log out

```sh
agentctl github logout default
```

Agents asking for `default` lose GitHub access at once. The token itself keeps working on GitHub until it expires, so
delete it in GitHub's [token settings](https://github.com/settings/personal-access-tokens) when you no longer need it.
