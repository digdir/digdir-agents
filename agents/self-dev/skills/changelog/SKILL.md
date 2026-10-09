---
name: changelog
description: Write and edit agentctl/CHANGELOG.md entries as release notes for the people who use agentctl and its Agents. Use when adding or changing a changelog entry, when a pull request needs one, or when preparing a release's changelog.
---

# Changelog entries

A changelog is release notes for the people who use the product. Pull request titles and descriptions are for the
people who review the code. Do not copy one into the other: a reviewer needs to know how and why, a reader of the
changelog needs to know what changed for them and whether they must act.

## Rules

- **One entry per change a reader notices**, however many pull requests it took. Do not join unrelated changes in one
  sentence, even when one pull request made them: make them separate entries, or sub-bullets under what they change.
  If `[Unreleased]` already has an entry for the same feature, extend or rewrite that entry instead of adding another.
  No entry for refactors, tests or CI, or for a change an `[Unreleased]` entry already describes with its issue
  linked: apply the `skip-changelog` label.
- **Short.** One or two sentences, 40 words or fewer as a rule, and never more than 60.
- **Lead with what changed for the reader**, then say what they can do now or what they must do.
- **Use the names readers know**, written exactly as they appear in the product, so the entry can be searched. Give a
  few examples rather than a complete list.
- **Leave out** how it is implemented, why it was designed that way, internal components, and what used to happen,
  unless the reader must act on it.
- **Fixed** entries describe the symptom the reader saw, not the cause.
- **Breaking changes, deprecations and removals** say what to do instead, and breaking changes start with `Breaking:`.
  Step-by-step migration belongs in the documentation, such as `agentctl/README.md`; link to it. Say so when a tool,
  such as `agentctl self update`, makes the change for the reader.
- **End with the references in parentheses**: the documentation link first, when there is one, then every issue whose
  problem or request the entry describes, or the pull request when there is no issue:
  `([install](https://github.com/digdir/digdir-agents/tree/main/agentctl#install), [#1234](https://github.com/digdir/digdir-agents/issues/1234))`.
  Links do not count toward the word limit.
- **Link the issue, not its pull requests.** An issue says what readers asked for or ran into and leads to the pull
  requests for it, so link it once, however many pull requests it took. When the issue is part of a larger one about the
  same change, such as a feature, link the larger one, but not an issue that gathers unrelated work, such as an epic or
  a list of findings. Do not link an issue the change is only related to. Write an issue or pull request in another
  repository as `[digdir/microsandbox#12](https://github.com/digdir/microsandbox/pull/12)`.
- **Without an issue, link the pull request.** Do not open an issue afterwards to have one to link.
- **Sub-bullets group changes by what readers know them by**, such as a command, a manifest field or a view in
  `agentctl tui`: the top line names it, and each sub-bullet is one change to it. Use one level, at most five short
  sub-bullets, and put each reference on the line it belongs to, or on the top line when it covers every sub-bullet.
  The word limit counts the whole entry, sub-bullets included. Changes in different categories are separate entries.
- **Do not wrap lines.** Only sub-bullets start a new line within an entry.

Before you finish, read the entry as someone who has only the changelog: can they tell what changed for them and
whether they need to do anything? Delete every clause that does not help with that.

## Writing the entry for a pull request

You know the implementation too well to see it from the reader's side. Write the entry from what the reader will
notice after upgrading, not from what you did:

1. Decide whether the change is visible to the changelog's readers at all, and whether an `[Unreleased]` entry already
   describes it with its issue linked. In either case, use the `skip-changelog` label.
2. If your harness can start a subagent, give a fresh one only this skill, the pull request title and description, and
   the diff of what the reader sees or uses, and have it draft the entry. Otherwise, write the entry before rereading
   the implementation.
3. Merge it with any related `[Unreleased]` entry, keeping that entry's references. Link the issue for this pull
   request, as the rules above describe, and reference it in the pull request description (`Closes #1234`, or
   `Part of #1234`) so readers can get from the issue to the change. Without an issue, add this pull request's link
   once it is open.
4. Commit, then check the changelog with `agentctl/changelog.sh validate` and
   `agentctl/changelog.sh check-unreleased origin/main HEAD`.

## Preparing a release

Read the entries being released together. If they follow the rules above, promote them as they are, as
`agentctl/AGENTS.md` describes. Otherwise, fix them in the promotion pull request:

- Merge entries about the same feature, keeping all their references. Drop a pull request link when the merged entry
  links the issue for that pull request.
- Replace a pull request link with the issue the rules above point to, when there is one.
- Drop entries for something added and fixed within the same release: readers never saw the problem.
- Cut entries to the rules above, and move migration detail to the documentation.

## Examples

Each block shows entries exactly as they are written in the changelog. Long entries are cut short with `...`.

Too long, with implementation detail:

```markdown
- `agentctl stop` and `agentctl start` change the Agent's desired run state, which the Sandbox controller reconciles by asking the Microsandbox backend to stop the VM process while keeping the root filesystem and volumes, so that a later start reuses the same disk instead of materializing the image again. ...
```

Better:

```markdown
- `agentctl stop` and `agentctl start`, or `x` in `agentctl tui`, stop an Agent's VM and start it again on the same disk, also one that stopped responding. ([#123](https://github.com/digdir/digdir-agents/issues/123))
```

Explains the mechanism instead of the effect:

```markdown
- On macOS, the network backend answers DNS queries for host-default names by calling the system resolver instead of forwarding them to the servers in `/etc/resolv.conf`.
```

Better:

```markdown
- On macOS, Agents resolve names through the host's system resolver, so VPN split DNS and `/etc/resolver` domains work inside an Agent as they do on the host. ([#123](https://github.com/digdir/digdir-agents/pull/123))
```

Several entries for one feature:

```markdown
- `agentctl tui` opens an Agent's shell, VS Code, Zed or SSH with `o`. ...
- `agentctl tui` opens an Agent's desktop in the browser or a VNC client with `o`. ...
- `agentctl tui` opens a port forward with `o` from the forwards view. ...
```

Better, as one entry with sub-bullets:

```markdown
- `agentctl tui` opens an Agent with `o`:
  - in a shell, VS Code, Zed or SSH, offering to add the `Include` to `~/.ssh/config` first ([#123](https://github.com/digdir/digdir-agents/pull/123))
  - on its desktop, in the browser or a VNC client ([#124](https://github.com/digdir/digdir-agents/pull/124))
  - through a forward, from the forwards view ([#124](https://github.com/digdir/digdir-agents/pull/124))
```

Two changes in one entry:

```markdown
- `agentctl tui` asks before quitting when that would close port forwards, and forwards are now closed when their Agent is deleted or re-created.
```

Better, grouped under what they change:

```markdown
- `agentctl tui` port forwards: ([#124](https://github.com/digdir/digdir-agents/pull/124))
  - `q` asks before quitting would close them
  - they close when their Agent is deleted or re-created
```

A fix described by its cause:

```markdown
- `agentd` no longer blocks its request loop while a cancelled client waits for an Agent to become ready.
```

Better, by its symptom:

```markdown
- Interrupted commands that wait for an Agent, such as an editor retrying its SSH connection to an Agent that cannot start, no longer make `agentd` stop answering every other command. ([#123](https://github.com/digdir/digdir-agents/pull/123))
```
