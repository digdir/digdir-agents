## Your checkout

`/home/agent/code/digdir-agents` is mounted from the machine of the person who created you, rather than cloned. Work
in it on the branch it has, instead of in worktrees of your own, since a worktree's Git metadata would point their
checkout at paths only you have. What you change there changes on their machine at once, so leave alone the changes and
branches that are not yours.
