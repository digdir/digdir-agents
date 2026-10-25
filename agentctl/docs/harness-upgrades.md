# Harness upgrades

A harness version bump is an adapter change. The unit tests and transcript fixtures only show that the adapter worked
with the version they were recorded from, and `agentd`'s version check only confirms that the image has the pinned
version. Only a live run shows whether the adapter still handles the new version's terminal, transcripts, hooks, login
and resume.

An existing Agent keeps the image it was created with, but `agentctl self update` gives it the new `agentd` at once.
Adapter changes that accompany a bump must therefore also work with the previous pin.

The adapters live in [`src/harness`](../src/harness). Update every pin of a harness together. `git grep` its previous
version to find them.

## Upgrade test plan

Test with `agentctl` and the self-dev image both built from the branch: run `make user-install`, then apply the
self-dev `nested` variant on base images built from the branch, as
[their README](../../images/agentctl-base/README.md#changing-them) describes. `make user-install` restarts `agentd` and
refuses while a Session is `Working`, so archive or delete earlier test Sessions first. Use fresh Sessions and check
`claude --version`, `codex --version` and `pi --version` in the Sandbox. An existing Sandbox does not prove the
rebuilt image works.

Run each check against Claude Code and Codex ("Both") unless the table names one, and against Pi as [below](#pi). A
low-cost model is enough, but Sessions on each installation's manifest `defaults` must work at least once. Before
attributing a failure to the bump, repeat the check on an Agent built with the previous pin.

`agentctl` has no command for some of the steps:

- Keys without attaching: `agentctl exec agent/<agent> -- tmux send-keys -t '=agent-session-<id>:' Escape`, with the
  Session `id` from `agentctl get sessions -o json`. `tmux capture-pane -p -t '=agent-session-<id>:'` prints the screen.
- Idle without waiting 30 minutes: `agentctl archive`, then `agentctl unarchive`; `agentctl create` or `attach` then
  resumes the Session. Run the real idle-stop once, in the background.
- A long foreground tool call: ask for `timeout 90 tail -f /dev/null`. Claude Code refuses a bare `sleep` and may run a
  command in the background instead.
- Claude Code Bash approvals: add `"permissions": {"ask": ["Bash(touch *)"]}` to the project's
  `.claude/settings.local.json` in the Sandbox, start a new Session and ask it to run `touch`. The platform's
  `PermissionRequest` hook approves the call,
  including requests that Claude's deletion checks raise despite `--dangerously-skip-permissions`. Confirm that
  `prompt --wait` waits for the completed turn rather than returning `WaitingForInput` at the approval.
- Claude Code outside a Session, as T3 Code runs it: in `agentctl ssh`, run `mkdir -p /tmp/x/a`, write
  `{"type":"user","message":{"role":"user","content":"Run exactly: sh -c 'cd /tmp/x && rm -rf $(echo a)'"}}` to
  `/tmp/x/prompt.jsonl`, and pipe `(cat /tmp/x/prompt.jsonl; sleep 60)` into
  `claude -p --input-format stream-json --output-format stream-json --verbose --permission-prompt-tool stdio`, first
  with `--permission-mode bypassPermissions --allow-dangerously-skip-permissions`, then with
  `--permission-mode default`. Claude Code needs its input open to receive the answer to a permission request.
- A Claude Code permission prompt: use `"permissions": {"ask": ["Write"]}` instead, and ask for a file written with
  the `Write` tool. Other tool permissions still wait for the operator. Restore the file afterwards. The
  `AskUserQuestion` tool blocks on the operator without any configuration.
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
| Bash permission request, including a deletion safety check          | Claude Code | Runs without operator approval; `prompt --wait` ends only when the turn completes. Repeat in a background subagent. |
| Deletion safety check outside a Session                              | Claude Code | In bypass mode, the tool result's `permission_decision` comes from the `hook`, any `can_use_tool` request is followed by its `control_cancel_request`, and `/tmp/x/a` is gone. In default mode, the `can_use_tool` request waits for the client. |
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
| Home overlay replaces Claude's user settings                         | Claude Code | With no legacy bypass acknowledgement in the image and no acknowledgement in the overlay, create, prompt, re-apply, archive and resume repeatedly. The platform's `agent-settings.json` keeps the acknowledgement; the overlay's settings remain unchanged. |
| Nested Agent                                                         | Both        | Inside a Session, `agentctl claude login` and `agentctl codex login` let a nested Agent run both harnesses.                                                                                                             |
| New `agentd` with the previous pin                                   | Both        | When the change touches adapter code, an Agent on the previous image still creates, prompts, reads turns and resumes.                                                                                                   |

## Pi

Test Pi on an Agent that configures it, such as [self-dev](../../agents/self-dev/README.md#pi). The checks above apply,
with these differences:

- Pi asks for no permissions; only an extension dialog reports `WaitingForInput`.
- Escape completes the turn, and `turns` marks the aborted call failed. A queued prompt returns to the editor, so the
  next prompt appends to it.
- A model error shows in `turns` as `Error: …`; an attempt Pi retried successfully does not. Known gaps: a prompt Pi
  rejects before starting a run, for a missing API key or model, and a failure in run setup after `before_agent_start`
  report no completion, so a wait times out. A missing-key rejection can leave STATE at `WaitingForInput`; a setup
  failure after `before_agent_start` leaves it at `Working`. A `--model` whose provider Pi does not know makes Pi exit
  at startup.
- Resume opens the reported transcript only when it is non-empty; otherwise the Session starts fresh.
- An initial prompt starting with `@` gets a leading space, since Pi reads `@name` as a file.
- `spec.instructions` reach the model from `~/.pi/agent/AGENTS.md`, and the Agent's other `~/.pi/agent/` files are
  unchanged after `agentd` reconciles the Agent again.

## Judging and reporting

Inspect `get sessions` (including `-o json`) and `turns` alongside the terminal. Check user messages, assistant answers,
tool results and turn boundaries, including after compaction. A successful model response alone is insufficient.

Also run `cargo test -p agent --lib -- --ignored`, part of `make test-e2e`, on a host with Node.js and tmux. It drives
tmux with a synthetic program, not the harnesses, so it does not replace the live checks. Record in the pull request,
for each check and harness, the result or why it was not run, with the tested versions. Update adapter fixtures when
native output changes. Never paste credentials into the pull request, including command lines that carry a token.
