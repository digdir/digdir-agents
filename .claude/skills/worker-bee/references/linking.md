# Finding and linking work

## Finding existing work

Look before you start on an issue, and again right before you open a pull request. Others may be on it in a draft.

- `gh issue view N --json assignees,closedByPullRequestsReferences` lists who is assigned and the pull requests
  linked to the issue, drafts included. `gh issue view --comments` does not show the linked ones.
- `gh pr list --state all --search "N in:body,title"` finds the ones that only mention it. A closed one tells you
  what was tried and why it stopped.
- `gh issue develop --list N` lists the branches linked to it.
- `gh pr list --json number,title,files` lists the files each open pull request changes. One that changes the files
  you will change may solve the issue on the way, or conflict with your fix.
- Read the thread for a comment that claims the issue, however old. If the claim looks stale, say so rather than
  taking the issue.

## Linking a pull request

The link is how others find the pull request from the issue, so every pull request for issue N has one.

- Start the branch with `gh issue develop N --name BRANCH --base main --checkout`. GitHub links the pull request you
  open from it.
- In the description, write `Closes #N` when the pull request finishes the issue and `Part of #N` when it does not.
  `Closes #N` there also makes the link, but not in a commit message.
- Both ways link a pull request only while it targets `main`. One stacked on another branch is linked once it is
  retargeted, or before that by a person, in the issue's Development field.
- After `gh pr create` or `gh pr edit`, check the link with the first command above.

## Relations between issues

Record how issues relate as relations, in addition to saying it in the text. Everyone sees a relation on the issue
and in the project, while a mention reaches only those who read that text.

- `gh issue edit N --parent M` makes N a sub-issue of M, when it is part of a larger one.
- `gh issue edit N --add-blocked-by M` says that N can't go on before M is done. It takes a URL for an issue in
  another repository.

Remove a relation when it no longer holds.

## Assignment

Assign yourself to the issue when you start on it with `gh issue edit N --add-assignee @me`, and to each pull
request you open for it with `gh pr edit --add-assignee @me`. That shows others someone is on it, and who to ask.
