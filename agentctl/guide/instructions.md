# Instructions

An agent's harness reads its instructions at the start of every session: first the Agent sandbox section, written
by agentd for every agent, then the files the manifest's `instructions` lists, in that order, usually only
`agents/<name>/instructions.md`.

The section already lists the agent's computer (user `agent` with passwordless `sudo`, working in `~/code`), its
CPUs, memory and disk, its harnesses, the folders mounted from the person's machine, its secrets and the hosts they
go to, its other environment variables, its network, its GitHub account and how to see which it is, how people reach
it, and whether it can run nested agents. Then its ground rules: change anything inside the VM but act outside it
(pushes, pull requests, comments) only when a task asks, note system changes and why in `~/system-changes.md` so they
can move into the image, remove what it created for a task, check at the start of a session for the tools and
repositories its own instructions list and install or clone what is missing, and in clones fetch first, branch from
the default branch, never discard work it didn't make and push what should last. Don't repeat any of that.

## The agent's own instructions

Keep them short: they cost context in every session. Say what the agent is for, then list its repositories and
tools, whether or not the image already has them:

```markdown
# Builds and tests the case-handling web app

## Repositories
- digdir/case-web in ~/code/case-web: the app
- digdir/case-api in ~/code/case-api: the API it calls; read it, don't change it

## Tools
- Node.js 22 with pnpm 9
- .NET 9 SDK
- Playwright with Chromium
```

A repository mounted from the person's machine is listed with the folder it is mounted at. Add only what the person
always wants this agent to do or never to do, in a line each, such as "Open pull requests as drafts". What a
repository's own AGENTS.md or README says, the harness reads there.

Don't write which repositories the agent can reach or push to. The token behind its GitHub account decides that, it
can change at any time, and you can't see it; the section already says how GitHub works for the agent.
An agent for a folder rather than a repository can say what it clones on demand and which tools to install when a
task needs them, instead of listing everything.

## Instructions the person wants in every agent

Someone may keep instructions they want in all their agents, such as how they track work, in a file of their own,
such as `~/agent-instructions.md`. List that file after the agent's own, with `optional: true`: the agent then still
starts on a machine without it, such as a colleague's who checks out the repository, and `agentctl describe
agent/<name>` shows that it was left out.

```yaml
instructions:
  - source: instructions.md
  - source: ~/agent-instructions.md
    optional: true
```

agentd reads the files again on every pass, so an edit reaches a running agent within a minute, and so does an
optional file created later. Adding a file to the list or removing one means creating the agent again.
