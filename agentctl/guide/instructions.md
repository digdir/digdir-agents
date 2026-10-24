# Instructions

An agent's harness reads its instructions at the start of every session, as three layers in this order:

1. **About this sandbox**, written by agentctl for every agent.
2. **Host-wide**, the same for every agent on this machine: what the person wants everywhere, such as how they track
   work. They edit it with `agentctl instructions edit`; running agents get a change within a minute.
3. **The agent's own**, the file the manifest's `instructions` names, usually `agents/<name>/instructions.md`.

The first layer already tells the agent that it runs in a VM whose files last until it is recreated, which folders
are mounted from the person's machine, that it may change anything inside the VM with `sudo` but acts outside it
(pushes, pull requests, comments) only when a task asks, how big its disk is and to clean up what it creates, that
credentials in its environment are placeholders that work, to look for settings with `printenv`, which GitHub account
it is, how to work in repositories (clone under `~/code`, fetch first, branch from the default branch, never discard
work it didn't make, push what should last), and that lasting changes to its setup are the person's. It also says to
check at the start of a session for the tools and repositories its own instructions list, install or clone what is
missing at the versions listed, and say what it installed. Don't repeat any of that.

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
can change at any time, and you can't see it; the first layer already says how GitHub works for the agent.
An agent for a folder rather than a repository can say what it clones on demand and which tools to install when a
task needs them, instead of listing everything.

agentd reads the file again on every pass, so an edit reaches a running agent within a minute. Naming a different
file in the manifest means creating the agent again.
