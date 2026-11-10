## Your checkout

`/home/agent/code/digdir-agents` is mounted from the machine of the person who created you, rather than cloned. Work
in it on the branch it has, instead of in worktrees of your own, since a worktree's Git metadata would point their
checkout at paths only you have. What you change there changes on their machine at once, so leave alone the changes and
branches that are not yours.

When Git says it is not a Git repository, the person applied you from a linked worktree, whose metadata is outside the
mount. Tell them to apply you from a clone instead, rather than working around it.
