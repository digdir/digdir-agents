# AGENTS.md

Follow [CONTRIBUTING.md](CONTRIBUTING.md).

- Read the code, not the docs, to learn how something works.
- Run `make fmt lint build test` before reporting a change complete.
- Before pushing to a pull request's branch, fetch its base branch and rebase on it.
- Before you start on an issue, and again right before you open a pull request, look for pull requests already made
  for it, so you build on or review them instead of opening a parallel one. Others may be on it in a draft. List the
  pull requests linked to the issue, drafts included, with `gh issue view N --json closedByPullRequestsReferences`,
  since `gh issue view --comments` does not show them. Then find the ones that only mention it with
  `gh pr list --state open --search "N in:body,title"`.
- In the description of a pull request for issue N, write `Closes #N`, or `Part of #N` when it solves only part of
  the issue, so merging it does not close the issue. GitHub links a pull request to its issue only from a closing
  keyword in the description, not from commit messages, and that link is how others find the pull request from the
  issue. After `gh pr create` or `gh pr edit`, check the link with the first command above.
- When you start on an issue, assign it to yourself with `gh issue edit N --add-assignee @me`, and each pull request
  you open for it, drafts included, with `gh pr edit --add-assignee @me`. That shows others someone is on it.
- The [digdir-agents project](https://github.com/orgs/digdir/projects/74) shows people where each issue and pull
  request stands, without reading every thread. GitHub adds them to it and sets Status to Done when they close. You
  keep the rest current on the issue and on its pull requests, in the same step as the change it describes, since a
  board that lags is read as the truth anyway:
  - Status is In Progress from when you start on the issue.
  - Agent State says where the work is: Analyzing while you read and plan, Coding while you write the change, QA while
    it is reviewed, and Blocked: Human needed while it waits on a person. Clear it when the work is merged or closed.
  - Blocked: Human needed also stops you. A person sets it to pause your work, so check it before every action others
    will see. While it is set, do nothing others will see except to say what you wait for, and resume when a person
    changes it.

  Get an item's id with `gh project item-add 74 --owner digdir --url URL --format json`, which returns the existing
  item, and the field and option ids with `gh project field-list 74 --owner digdir --format json`. Then set the field
  with `gh project item-edit`. This needs the `project` and `read:org` scopes and access to the project, which GitHub
  hides from accounts without it. If you can't set a field, tell the person and carry on, and set it once they have
  fixed the access.

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
