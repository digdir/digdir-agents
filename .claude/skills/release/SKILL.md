---
name: release
description: Prepare a release of agentctl and agentd in digdir-agents as a pull request for a person to merge, and tidy the project board once it is published. Runs only when a person asks for it by name, as /release.
user-invocable: true
disable-model-invocation: true
argument-hint: "[version]"
---

# Release

You prepare the release and a person publishes it. Merging the release pull request publishes the release and
creates its tag, and a tag can never be moved or reused. So follow this skill only when the person prompting you
asked for it by name, and never merge that pull request yourself.

The version asked for: `$ARGUMENTS`

If that line is empty or still shows a placeholder, propose a version from what `## [Unreleased]` in
`agentctl/CHANGELOG.md` holds, and have the person confirm it before you open anything.

## 1. Prepare the pull request

[CONTRIBUTING.md](../../../CONTRIBUTING.md#changelog-and-releases) describes the pull request that makes a release
and what the release workflow does with it. Open that pull request, with the entries brought in line as "Preparing
a release" in the [changelog skill](../../../agents/dev/skills/changelog/SKILL.md) describes.

Wait for its "Assemble release" check. When it fails, read the failing step's log and fix the cause in the pull
request. Then tell the person that it is ready to merge, with the version and the release notes it will publish.

## 2. After it is published

Do this when the person has merged the pull request and the release exists, which `gh release view agentctl/vX.Y.Z`
shows. If the release failed, CONTRIBUTING.md says what to do, and the person decides which of its ways out to take.

Archive the items in Waiting to be released on the [project](https://github.com/orgs/digdir/projects/74) that
closed before the release pull request merged. The release has shipped them, and the column then holds only what
the next release will ship. List the column with:

```sh
gh project item-list 74 --owner digdir --format json --limit 500 \
  --jq '.items[] | select(.status == "Waiting to be released") | [.id, .content.number, .content.title] | @tsv'
```

Compare each item's `closedAt`, from `gh issue view N --json closedAt` or `gh pr view N --json closedAt`, with the
release pull request's `mergedAt`. Archive the earlier ones with `gh project item-archive 74 --owner digdir --id ID`,
and leave the later ones for the next release.

Tell the person what you archived and what you left.
