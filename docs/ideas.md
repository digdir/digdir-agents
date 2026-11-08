# Ideas

Ideas worth keeping that are not concrete enough to build on today's platform. Each one came out of real use, mostly
of the orchestrator pipeline that was removed in October 2026 (archived as the tag
[`v2.0-pre-cleanup`](https://github.com/digdir/digdir-agents/tree/v2.0-pre-cleanup)). The issues they came from are
closed; the links are there for the background and the incidents behind each idea.

When an idea becomes concrete, open an issue for it and remove it here.

## Events from Slack and GitHub into Agents

`integrations/` still listens to Slack and GitHub, but nothing consumes its events. If it, or something like it,
becomes an entry point to `agentctl` Agents, these lessons from the pipeline apply.

### Ignore other bots before anything else

Events whose actor is a bot account (`[bot]` suffix or `user.type == "Bot"`) are not work orders. In the pipeline a
CodeRabbit progress message reached the agent as the PR body and was turned into an order to squash-merge a PR to the
wrong base. Filter bot actors next to the existing allowlist and self-trigger checks, with an explicit allowlist for
bots that may give orders. When there is no triggering comment, mark the fallback text as "issue/PR body, not a
request". This is a precondition for reconnecting events, not an optional improvement.
From [#76](https://github.com/digdir/digdir-agents/issues/76).

### Give each event its conversation

An event without the thread around it loses its meaning: "I commented on the PR, fix it please" was resolved against
the wrong repository. Include the last N messages of the thread or DM (`conversations.replies` for threads), mark them
as untrusted context, and fail open if fetching them fails. In `agentctl` terms, the better answer may be to route
every message in a thread to the same Session, so the harness keeps the conversation itself.
From [#65](https://github.com/digdir/digdir-agents/issues/65).

### One topic, one piece of work

The same issue was delegated twice within minutes, once from Slack and once from a comment on the issue. Before
starting work, check whether the topic is already being worked on, and answer with status and a link if it is. DMs
need a key per channel rather than per message. With Sessions this becomes a mapping from topic (thread, issue) to an
existing Session.
From [#67](https://github.com/digdir/digdir-agents/issues/67).

### Comments in threads the bot takes part in

Thanks, confirmations and follow-up questions in GitHub threads the bot has commented in were ignored, because only
`assign` and `mention` notifications were handled. Handling `comment` notifications, with a loop guard on the bot's own
account, gives GitHub the same feedback channel Slack has.
From [#42](https://github.com/digdir/digdir-agents/issues/42).

### Learn when a piece of work ends

When an issue closes or a topic goes stale, give the Agent a chance to reflect: what was the intent, was it reached,
what should be remembered. Keep a working note per topic while the work goes on, and turn the reflection into 0–2
learnings. Stale DM channels are context, not tasks, and get no retrospective.
From [#69](https://github.com/digdir/digdir-agents/issues/69).

## Observability

### Record which model answered, and fail loudly on empty runs

Models were switched often, and afterwards no one could say which model gave which answer. Stamp model, endpoint (host
only), duration and output size on each turn when it happens, so the history stays right when the model changes. A
run with no output, or that claims to be done without a single tool call, should be an error the requester sees, never
a silent success.
From [#71](https://github.com/digdir/digdir-agents/issues/71).

### Follow a topic across Agents

A single view from the triggering message through delegation to the PR, across Agents, without opening several
terminals. Read-only, with no tokens of its own, and with deep links into the Agent's editor. Related to web views an
Agent declares ([#387](https://github.com/digdir/digdir-agents/issues/387)) and the tray app
([#380](https://github.com/digdir/digdir-agents/issues/380)).
From [#98](https://github.com/digdir/digdir-agents/issues/98).

## Sessions

### Hand a Session between headless and hands-on

Three ways to drive the same work in progress: headless (prompted by automation), hands-on (a person attached to the
same Session or editor) and external (someone continuing the same branch on their own checkout). Switching needs a
lease per Session so prompts are not injected while a person drives, and a checkpoint at each handover: commit and push
before release, pull and catch up before resuming. The lease coordinates; it is not a security boundary. Related to
exporting a conversation to another Agent ([#339](https://github.com/digdir/digdir-agents/issues/339)).
From [#99](https://github.com/digdir/digdir-agents/issues/99).
