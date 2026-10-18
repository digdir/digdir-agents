# Harness compatibility

Treat harness installation version bumps as adapter changes: existing unit tests and transcript fixtures do not
establish compatibility with a new binary. Terminal behavior, transcript formats, hook semantics, authentication
and resume behavior all need live verification. Version verification checks identity, not compatibility.

An existing Agent keeps the image it was created with, but `agentctl self update` gives it the new `agentd` at once.
Adapter changes that accompany a bump must therefore also work with the previous pin.

Implementation: [adapters](src/harness), [terminal runtime](src/sessions/runtime/tmux.rs),
[Session service](src/sessions/service.rs). Harness pins: [self-dev](../agents/self-dev/Dockerfile) (the only Pi and
OpenCode pins), [minimal](examples/minimal/Dockerfile) (Claude Code only) and
[worktree](../sandbox/core/examples/worktree/Dockerfile); update them together.

## Upgrade test plan

Install the current platform with `make user-install` from the repository root; it replaces and restarts `agentd`, and
refuses while any Session reports `Working` (#149, #150), so archive or delete earlier test Sessions first. Test the
self-dev image built from the branch: from `agents/self-dev/minimal` in the branch's checkout,
`agentctl apply --variant nested --env-file <file> --wait` builds it with all four harnesses and fits inside another
Agent. Keep the env file outside the checkout. Use fresh Session names and confirm `claude --version` and
`codex --version` in the Sandbox; testing an existing Sandbox does not prove the rebuilt image works.

Run each check against both harnesses unless the table names one. A low-cost model is enough, but Sessions on each
installation's manifest `defaults` must work at least once. Before attributing a failure to the bump, repeat the check
on an Agent built with the previous pin.

`agentctl` has no command for some of the steps:

- Keys without attaching: `agentctl exec agent/<agent> -- tmux send-keys -t '=agent-session-<id>:' Escape`, with the
  Session `id` from `agentctl get sessions -o json`. `tmux capture-pane -p -t '=agent-session-<id>:'` prints the screen.
- Idle without waiting 30 minutes: `agentctl archive`, then `agentctl unarchive`; `agentctl create` or `attach` then
  resumes the Session. Run the real idle-stop once, in the background.
- A long foreground tool call: ask for `timeout 90 tail -f /dev/null`. Claude Code refuses a bare `sleep` and may run a
  command in the background instead.
- A Claude Code permission prompt: add `"permissions": {"ask": ["Bash(touch:*)"]}` to `~/.claude/settings.json` in the
  Sandbox, start a new Session and ask it to run `touch`; ask rules prompt despite `--dangerously-skip-permissions`.
  Restore the file afterwards. The `AskUserQuestion` tool blocks on the operator without any configuration.
- A daemon restart: `pkill -x agentd`; the next `agentctl` command starts it again.

| Check                                                                | Harness     | Expected result                                                                                                                                                                                                         |
| -------------------------------------------------------------------- | ----------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Create with an initial prompt                                        | Both        | One submission and answer; a daemon restart does not replay the prompt. A failed launch may lose the initial prompt and recover with an empty conversation.                                                             |
| Create without a prompt, then immediately `prompt --wait`            | Both        | Input submits without manual Enter. Repeat several times to expose startup races. Codex readiness depends on its `› ` composer and the truncated session-ID pane title.                                                 |
| Short, long, multiline, XML-shaped and literal request-heading input | Both        | `turns` preserves the complete operator input and shows no harness-injected text as operator input (#148).                                                                                                              |
| Prompt again after completion, including identical text              | Both        | Waits for one more completed turn, ignoring previous completions.                                                                                                                                                       |
| Prompt during an active tool call, including identical text          | Both        | Input appears in `turns`; waiting follows work observed during settling, but does not demand an extra turn when input is absorbed into the current one.                                                                 |
| Prompt while a background command runs, until it finishes            | Claude Code | The wait ends with the operator's turn; the completion notification's own turn does not end a later wait early.                                                                                                         |
| Tool success and tool failure                                        | Both        | STATE reflects activity; tool completion alone does not complete a turn; `turns` marks only the failed call.                                                                                                            |
| Permission prompt and `AskUserQuestion`, approved and denied         | Claude Code | While blocked, STATE is `WaitingForInput` and `activity.turns` does not advance; answering or approving completes the turn. A denial is an interruption (#147). Not tested for Codex, which launches without approvals. |
| Interruption during a tool call, then another prompt                 | Both        | Codex reports `Interrupt`, which releases the wait. Known gap (#147): Claude Code has no interrupt hook, so STATE stays `Working` and a wait times out. The next prompt works in both.                                  |
| Model error reported through `StopFailure`                           | Claude Code | Create with an unknown `--model`. The completion report ends the wait, but does not imply a successful model response; inspect `turns`.                                                                                 |
| Model error without a completion report                              | Codex       | Create with an unknown `--model`. Codex shows the API error without a turn ending, so the wait times out and STATE stays `Working` (#150); inspect the Session and recover manually.                                    |
| Short completion timeout                                             | Both        | Queuing, input readiness and delivery finish before the completion timeout starts. A timeout reports that the prompt was submitted; inspect turns before retrying. The next prompt contains no leftover draft.          |
| Idle-stop and resume, before and after the first turn                | Both        | After 30 unattached, quiet minutes the Session is Idle. An untouched Session remains usable; an established conversation resumes with its history.                                                                      |
| Archive mid-turn, then unarchive and resume                          | Both        | The harness stops once the turn ends; `create` or `attach` after unarchive resumes the conversation.                                                                                                                    |
| Idle Session whose transcript is older than 30 days                  | Claude Code | After a prompt in another Session of the Agent, it still resumes with its history. Backdate the transcript with `touch -d`.                                                                                             |
| Codex rollout compression                                            | Codex       | `local_thread_store_compression` is still off by default. Otherwise, check that `turns` and resume work for an Idle Session whose rollout is older than 7 days.                                                         |
| Create with `--model`/`--effort`, and with only manifest `defaults`  | Both        | `get sessions` shows the resolved selection and the harness reports the same model and effort, also after idle-stop and resume; an unknown value fails visibly in the terminal.                                         |
| Transcript location                                                  | Both        | `get sessions -o json` reports a `harnessTranscriptPath` that exists and grows with each turn, also after resume.                                                                                                       |
| Authentication and configuration                                     | Both        | Mediated login/inference works without unexpected onboarding or authentication dialogs; startup shows no new warnings; the status line renders; configured instructions and skills are available.                       |
| Nested Agent                                                         | Both        | Inside a Session, `agentctl claude login --from-stdin` with `$AGENT_CLAUDE_ACCESS_TOKEN` and `agentctl codex login --from-stdin < ~/.codex/auth.json` let a nested Agent run both harnesses.                            |
| New `agentd` with the previous pin                                   | Both        | When the change touches adapter code, an Agent on the previous image still creates, prompts, reads turns and resumes.                                                                                                   |

## Pi

Test Pi on an Agent that configures it, such as [self-dev](../agents/self-dev/README.md#pi). The checks above apply,
with these differences:

- Pi asks for no permissions; only an extension dialog reports `WaitingForInput`.
- Escape completes the turn, and `turns` marks the aborted call failed. A queued prompt returns to the editor, so the
  next prompt appends to it.
- A model error shows in `turns` as `Error: …`. Known gap: if Pi fails in run setup after `before_agent_start`, no
  completion is reported and STATE stays `Working`.
- Resume opens the reported transcript only when it is non-empty; otherwise the Session starts fresh.
- An initial prompt starting with `@` gets a leading space, since Pi reads `@name` as a file.
- `spec.instructions` reach the model from `~/.pi/agent/AGENTS.md`, and the Agent's other `~/.pi/agent/` files are
  unchanged after re-convergence.

## OpenCode

Test OpenCode on an Agent that configures it, such as [self-dev](../agents/self-dev/README.md#opencode). The checks
above apply, with these differences:

- Escape twice interrupts, as OpenCode's own hint says; the completion reports `interrupted`, and `turns` marks the
  aborted call failed.
- The question tool and permission prompts report `WaitingForInput`, also from a subagent; answering resumes the turn.
  `--auto` approves every permission the configuration does not deny.
- An unknown `--model` or `--effort` variant fails the request through the activity plugin, with the reason in the
  terminal and in `turns` as `Error: …`. Without the plugin OpenCode would silently use another model.
- A variant the operator picks in the TUI is kept in OpenCode's state and overrides the Session's effort in later
  launches; a resumed conversation keeps its own model.
- Check the model and effort of the first request, not only the footer, for example with a plugin `http.request` hook:
  the TUI picks a new conversation's model before it has read the configuration unless an agent carries one, which the
  activity plugin sees to.
- The transcript is OpenCode's database: `harnessTranscriptPath` names `opencode.db`, and `turns` reads the Session's
  rows from it through the OpenCode binary.
- The plugin reports nothing before the first prompt, so STATE stays `Starting` and input readiness comes from the pane:
  the prompt box's `┃` under the cursor and the title `OpenCode`, or `OC | <title>` when resumed.
- The TUI uses the alternate screen, so tmux keeps no scrollback of the conversation.

Completion waits poll local database activity every 250 ms and require identical completed, waiting activity
in two consecutive polls.

Inspect `get sessions` (including `-o json`) and `turns` alongside the terminal. Check user messages, assistant answers,
tool results and turn boundaries, including after compaction. A successful model response alone is insufficient.

Run the normal formatting, lint and test checks, plus `cargo test -p agent --lib -- --ignored`, which `make test-e2e`
and CI also run, on a host with Node.js and tmux. Those tests drive tmux with a synthetic program, not the harnesses, so
they do not replace a live check; the scrollback and Ctrl-Z checks also require Linux and util-linux `script`. Record in
the PR, for each check and harness, the result or why it was not run, with the tested versions and commands. Update
adapter fixtures when native output changes, and add a changelog entry. Never publish credentials or
authentication-bearing process arguments.
