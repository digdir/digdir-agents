# Onboard a repository

Give a repository an agent definition that everyone on the team starts the same way: the agent builder drafts it, you
check and try it, and a pull request makes it the team's. [The tutorial](first-agent.md) walks through the same steps
on an example, with the output of each.

You need `agentctl`, logged in to Claude Code and to GitHub under the login the definition will ask for, usually
`default`, as [Give an agent GitHub access](give-an-agent-github-access.md) describes. You need Docker to try the
definition before its image is published.

## Draft it with the agent builder

In a checkout of the repository:

```sh
agentctl new
```

The builder asks about the agent's role, what it should run to call a task done, whose agent it is, other repositories
it needs, and the services and secrets it uses. Answer "the team's" to whose agent it is. It then shows a plan, and once
you confirm, writes the definition, builds the image and tries it in a nested agent.

A team's agent goes in `agents/<role>/` in the repository, with a stage folder, such as `minimal/`, for each machine
it runs on, and a workflow in `.github/workflows/` that publishes its image. Its manifest references that image, so
teammates need no Docker. A personal agent goes in `~/agents/<name>/` instead, and builds its image on your machine.

The builder keeps running until you delete it. `agentctl new` in the same checkout goes back to the same
conversation, so you can ask it to change the definition later.

## Check the draft

The builder ends with what it verified and what it could not. Go through what it could not, and check these yourself:

- **The image name and the size.** The manifest's `image.reference` is where the workflow publishes, and
  `resources` is what every teammate's machine must have room for.
- **The pinned versions.** Each tool should be at the version the repository pins, in `global.json`, `.nvmrc` or its
  CI, so the agent gets the results CI gets.
- **What the agent may reach.** `accounts`, `secrets`, `network` and the hosts each secret may go to. Widening these
  is a decision for the team, not for the builder.
- **The workflow's permissions.** Only the jobs that push the image need `packages: write`.
- **The `.gitignore` in each stage folder.** It keeps personal variants, such as `agent.mine.yaml`, out of Git.

## Try it

The image doesn't exist until the workflow has run on the default branch. Until then, start the `nested-build`
variant, which builds it from your checkout:

```sh
cd agents/<role>/minimal
agentctl apply --variant nested-build --wait
```

Check that the agent cloned the repository and that its GitHub account may push there:

```sh
agentctl ssh agent/NAME -- ls code
agentctl ssh agent/NAME -- gh api repos/OWNER/REPO --jq .permissions
```

`"push": true` means the agent can push branches and open pull requests. `"push": false` means the login's token or
its account can't, which [Give an agent GitHub access](give-an-agent-github-access.md#when-the-agent-cannot-push)
explains. Then run a session in it and ask for what its instructions promise, such as the build and the tests.

## Make it the team's

Commit the definition and the workflow on a branch and open a pull request. Say in it what you and the builder
verified, and what not.

Once it is merged, the workflow publishes the image. GitHub makes a new package private, so make it public in the
package's settings, or every teammate needs a GitHub login that can read packages to pull it.

## What teammates run

Each teammate logs in once and applies the manifest from their own checkout:

```sh
agentctl github login default
agentctl apply -f agents/<role>/minimal/agent.yaml --wait
```

The agent acts as the GitHub account behind their own login, not yours. To change something for themselves only, such
as more memory for an IDE, they add a variant beside the manifest, as
[Open an agent in a JetBrains IDE](open-in-jetbrains.md#give-the-agent-room-for-the-ide) shows.

## Change it later

- **Instructions, skills, size, secrets and network** change in place. Change the file and run the same `apply`.
- **The image, mounts and harnesses** are fixed when the agent is created. `apply` refuses a change to them, and the
  agent must be deleted and applied again, which deletes its disk.
- **A new published image** reaches an agent only when it is created again, in the same way.

A builder started by an older `agentctl` keeps the image it was made with. `agentctl new` says so when you go back to
it, and to try the definition with your current `agentctl`, delete the builder and run `agentctl new` again. What it
wrote stays in the checkout.
