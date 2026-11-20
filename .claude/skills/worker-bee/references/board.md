# The project board

The [digdir-agents project](https://github.com/orgs/digdir/projects/74) shows people where the work stands, without
reading every thread. Keep it current in the same step as the change it describes, since a board that lags is read
as the truth anyway.

## One item for what lands together

An issue stands for the pull requests linked to it, whether one or several that merge together. GitHub adds every
pull request to the board, so archive the linked ones. Otherwise the same work shows twice, in different states.

A sub-issue that can land without its siblings, or in another release, stays beside its parent. So does a pull
request that is not linked to an issue, since nothing else on the board shows it.

## Status

Status says how far the work has come:

- **Analyzing** when you start to read and plan.
- **Building** when you write the change.
- **Testing** while the change is checked, by the checks you start and by CI.
- **Waiting to be merged** when all that is left is something that holds the merge, such as a person reading the
  draft or a code owner's approval.

GitHub sets **Waiting to be released** when the work is merged or closed. That includes work closed without a
merge, so archive it then, since no release will ship it. The [`release` skill](../../release/SKILL.md) archives
what a release has shipped.

## WIP State

WIP State says whether the work is moving right now, which Status can't show. It is **Working** while a session is
on it. Otherwise it says what the work waits for:

- **Up-for-grabs** when anyone can pick it up.
- The **Waiting** state that fits when it waits on another issue or on something outside the repository. Name what
  it waits on with a [blocked-by relation](linking.md#relations-between-issues).
- **Blocked: Human needed** when only a person can move it on. Say on the issue or pull request what you need from
  them, since a flag that doesn't say what it asks for is left unanswered. A person also sets it to pause an
  agent's work.
- **Other/Limbo** when none of these fit, with a comment that says what is going on.

Clear the field when the work is merged.

## Commands

Read an item's fields with [`scripts/work-state.sh N`](../scripts/work-state.sh). Set one with:

```sh
gh project item-edit 74 --owner digdir --url URL --field "WIP State" --value Working
```

URL is the issue's or the pull request's. Use `--clear` in place of `--value` to clear the field. With
`--format json` the command prints the item's id, which `gh project item-archive 74 --owner digdir --id ID` takes.

An older `gh` has no `--url` on this command, and no `--type`, `--parent` or `--add-blocked-by` on `gh issue edit`.
`work-state.sh` fails when that is the case, and updating `gh` fixes it.
