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
- When you start on an issue, assign it to yourself with `gh issue edit N --add-assignee @me`, and assign each pull
  request you open for it, drafts included, with
  `gh api -X POST repos/{owner}/{repo}/issues/PR/assignees -f "assignees[]=$(gh api user --jq .login)"`. Use the API
  for the pull request because `gh pr edit` also needs the `read:org` scope. Then set the issue's Status to
  In Progress in the projects it is on. That shows others someone is on it, even before a pull request exists.
  Setting the status needs a token with the `project` scope, and you need access to the project, since GitHub hides
  a private project from accounts without it. Set it with the `updateProjectV2ItemFieldValue` mutation through
  `gh api graphql`, because `gh project` also needs `read:org`. If you can't set it, tell the person and carry on
  with the work, and set the status once they have fixed the access.

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
