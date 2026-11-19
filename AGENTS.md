# AGENTS.md

Follow [CONTRIBUTING.md](CONTRIBUTING.md).

- Read the code, not the docs, to learn how something works.
- Run `make fmt lint build test` before reporting a change complete.
- Before pushing to a pull request's branch, fetch its base branch and rebase on it.

## Working on issues

### What to take first

When you choose what to work on, take what is closest to merged first, which is from right to left on the
[board](#the-project-board). Finishing a review or getting a pull request merged delivers something, while starting
on a new issue only adds to what is under way.

Analyze an issue together with the rest of the board, since a choice that is right for one issue can pull against
another. Read the other issues that touch the same area or the same kind of problem, those in Inbox as well as those
under way, and then:

- Split the work into parts that can each land without waiting for other issues.
- Don't solve alone a problem that another issue shares. The two need not be solved together, but your choice should
  fit what we know we will do later. Say on the issue how they relate.

### What you decide yourself

Checks show that a change works, not that the project should have it. That question weighs more the more of the change
users will meet, so the issue's type sets how far you take it on your own:

- A **Feature** changes what users see or build on, such as a new command or anything breaking. A person decides
  whether it should be done, even when it is easy to build. Analyze it, write what you would build and what there is to
  decide as a comment on the issue, and set Blocked: Human needed before you build. Go on without that only when a
  person has already made the decision on the issue.
- A **Bug** you can reproduce is yours to take from Inbox to merged without asking, and so is an **Enhancement**,
  which improves what is there without changing what users see or call, such as its performance. The review and the
  approvals a pull request needs still apply. A bug you can't reproduce is not yours to fix alone, since nothing shows
  that the fix fixed it. Say on the issue what you tried, and ask.

Set the type when you open an issue, and on an issue without one before you start on it, with
`gh issue edit N --type Bug`. When the type doesn't settle it, or the work turns out to change more than the type
says, such as an enhancement that needs a new flag, ask what users will notice. Set the type that fits and follow it.

Leave an issue labelled `no-agent` alone, and stop if the label is added while you work, since a person has kept it
for people. If you are asked to work on one, say that it carries the label.

### Finding and linking work

- Before you start on an issue, and again right before you open a pull request, look for pull requests already made
  for it, so you build on or review them instead of opening a parallel one. Others may be on it in a draft. List the
  pull requests linked to the issue, drafts included, with `gh issue view N --json closedByPullRequestsReferences`,
  since `gh issue view --comments` does not show them. Then find the ones that only mention it with
  `gh pr list --state open --search "N in:body,title"`.
- In the description of a pull request for issue N, write `Closes #N`, or `Part of #N` when it solves only part of
  the issue, so merging it does not close the issue. GitHub links a pull request to its issue only from a closing
  keyword in the description, not from commit messages, and that link is how others find the pull request from the
  issue. After `gh pr create` or `gh pr edit`, check the link with the first command above.
- Record how issues relate as relations, in addition to saying it in the text. Everyone sees a relation on the issue
  and in the project, while a mention reaches only those who read that text. Make an issue that is part of a larger one
  its sub-issue with `gh issue edit N --parent M`. When an issue can't go on before another is done, say so with
  `gh issue edit N --add-blocked-by M`, which takes a URL for an issue in another repository. Remove a relation when it
  no longer holds.
- Open every pull request as a draft, and mark it ready when the work is done and checked. A ready pull request
  brings in reviewers, and tells others that it is stable enough to build on and will rarely be rewritten. A draft
  says it may still change under them.
- When you start on an issue, assign it to yourself with `gh issue edit N --add-assignee @me`, and each pull request
  you open for it with `gh pr edit --add-assignee @me`. That shows others someone is on it. Assign yourself as well
  to work you follow up while another agent does it, so that agent and others know who to ask.
- A bug or an enhancement you find while working on something else can go straight to its own pull request, without
  an issue first, since the pull request can say all that an issue would. Say in its description what you were working
  on when you found it. A feature you find becomes an issue, since a person decides on it before anyone builds it.

### The project board

The [digdir-agents project](https://github.com/orgs/digdir/projects/74) shows people where the work stands, without
reading every thread. Keep it current in the same step as the change it describes, since a board that lags is read as
the truth anyway.

- The board shows one item for what lands together, and one for each thing that can land on its own. An issue stands
  for the pull requests linked to it, whether one or several that merge together. GitHub adds every pull request, so
  archive those. Otherwise the same work shows twice, in different states. A sub-issue that can land without its
  siblings, or in another release, stays beside its parent. So does a pull request that is not linked to an issue,
  since nothing else on the board shows it.
- Status says how far the work has come. Set Analyzing when you start to read and plan, Building when you write the
  change, Testing when it is ready for review, and Waiting to be merged when all that is left is something that
  holds the merge, such as a code owner's approval. GitHub sets Waiting to be released when the work is merged or
  closed. That includes work closed without a merge, so archive it then, since no release will ship it.
- WIP State says whether the work is moving right now, which Status can't show. It is Working while a session is on
  it. Before your session leaves the work, set what it now waits for, since a Working that no session stands behind
  keeps others off it:
  - Up-for-grabs when anyone can pick it up, such as a pull request ready for review.
  - The Waiting state that fits when it waits on another issue or on something outside the repository. Name what it
    waits on with a blocked-by relation.
  - Blocked: Human needed when only a person can move it on, such as a pull request that lacks only a code owner's
    approval. Say on the issue or pull request what you need from them, since a flag that doesn't say what it asks
    for is left unanswered.
  - Other/Limbo when none of these fit, with a comment that says what is going on.

  Clear the field when the work is merged.
- Blocked: Human needed also stops you. A person sets it to pause your work, so check it before every action others
  will see. While it is set, do nothing others will see except to say what you wait for, and resume when a person
  changes it.

Set a field with `gh project item-edit 74 --owner digdir --url URL --field "WIP State" --value Working`, where URL is
the issue's or the pull request's, and clear it with `--clear` in place of `--value`. The command prints the item's
id with `--format json`, which `gh project item-archive 74 --owner digdir --id ID` takes. This needs the `project` and
`read:org` scopes and access to the project, which GitHub hides from accounts without it. If you can't set a field,
tell the person and carry on, and set it once they have fixed the access.

## Writing

This applies to what you write for others to read, such as:

- docs and skills
- code comments
- commit messages
- issue and pull request descriptions and comments

It does not apply to your conversation with the person prompting you.

- Say each thing once, and link to where it is already said. That includes the code: a version, a default or a list
  of tools belongs where it is defined.
- State the rule _and_ why it exists, not a list of the cases it covers. Such lists go stale as the code changes.
- Describe things by what they do for the reader, not by how they are built today.
- Write the way you would say it to a colleague. Use periods rather than semicolons or em dashes.
- When you fix a problem in a text, check the rest of it for the same problem.
- Write for the reader, not as a record of your work. Leave out the steps you took, what you tried first, who found
  what and lists the diff already shows, such as every test you added. The person prompting you has already seen
  your process. The reviewer has the diff. What they need from you is what it can't show, such as why the change
  looks like this. Notes on your process dilute that.
- When more work changes a pull request, rewrite its description to describe the change as it now is, instead of
  appending to it. Appended rounds turn it into a history the reviewer has to replay to learn the result. A short
  comment can say what changed since the last review.
