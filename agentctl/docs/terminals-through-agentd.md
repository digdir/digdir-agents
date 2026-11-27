# Terminals through agentd

`agentd` runs every terminal a client shows in an agent and relays it over the Control API: `agentctl attach`,
`agentctl exec -it`, the TUI's attach and shell, and the sandbox sign-in. A client needs nothing but the Control API
socket.

## Why

- **A client should not need to know the sandbox.** Opening a terminal in Microsandbox takes the sandbox's Provider
  assignment, the path of `agentd`'s home and the Provider linked into the client, and opens `agentd`'s Microsandbox
  state from a second process. The Control API is meant to be the whole contract between a client and `agentd`.
- **A Provider is supported once.** `agentd` already opens sandboxes for everything else, so a second Provider needs no
  change in any client, the web GUI's server among them.
- **Detaching needs no keys.** A browser tab that closes, a process that gets killed, or a TUI that wants its screen
  back has no keys to send. With `agentd` running the terminal, closing the connection is enough.
- **Clients share one relay.** Each CLI client relays its terminal with the same code, and the web GUI relays the same
  frames.

## How a terminal crosses the Control API

The Control API is JSON-RPC, one JSON object per line, and a client opens a connection per call. A call that opens a
terminal upgrades its own connection: `agentd` starts the terminal and answers with an ordinary JSON-RPC response, and
from then on the connection carries terminal frames both ways until it closes. A refusal, such as a session that is
archived, is an ordinary JSON-RPC error, and the connection stays JSON.

This fits how the Control API already works. A client keeps a connection open for as long as what it asked for lives,
as `Client::run` does for an ephemeral agent with its `AgentLease`. The server serves each connection in its own loop,
so handing it to a relay after one response changes nothing else. And closing the connection is the one thing every
client does, however it ends.

Two alternatives were weighed. JSON-RPC notifications would grow every output byte by a third in base64 and need the
server to send messages it never sends today. A second connection, redeemed with a ticket from a first call, would add
a round trip and a ticket store, and protect nothing, since only the user can reach the socket anyway.

Both ends read past the response line: frames can follow it at once, so neither drops what its buffered reader holds.

### Frames

After the response, each frame is one type byte, a 32-bit big-endian payload length, and the payload. Output is
arbitrary bytes that can end in the middle of a UTF-8 character, so it travels as it is, unencoded. A frame's payload
is at most 64 KiB, and `agentd` splits larger output. `agentd` never interprets the output, which comes from the
untrusted sandbox. It only cuts it into frames.

| Type | From | Payload | Meaning |
| --- | --- | --- | --- |
| `input` | client | bytes | Bytes typed or pasted, as the terminal sends them |
| `resize` | client | rows and columns, each a big-endian `u16` | The client's terminal changed size |
| `detach` | client | none | The client is leaving and wants the terminal reset first |
| `output` | `agentd` | bytes | What the program drew |
| `ended` | `agentd` | JSON: `{"exited": code}`, `{"detached": true}` or `{"error": message}` | The last frame before `agentd` closes the connection |

A plain byte stream, as `docker attach` uses with a TTY, would need side channels for the size and the exit status.
Framing keeps everything on the one connection.

## What agentd runs

Two methods upgrade their connection:

- `sessions.v1.attach` with the agent, the session name and the client's terminal size. `agentd` starts a tmux client
  in the sandbox as a terminal Execution at that size. The result is the `Session`, so a client can title its window.

  Attaching never creates a session. That stays with `sessions.v1.ensure`, which takes the harness, model and initial
  prompt, so a client that may create one calls it first, and a client that only shows sessions needs none of that.
  Attaching does resume an Idle session and wait for a Resuming one, as `sessions.v1.prompt` does, so a session that
  went Idle between the two calls is no error.
- `agents.v1.terminal` with the agent, a command and the size, for terminals that are not a session's, such as a
  shell or a sign-in. The command gets the environment and folder every command in the sandbox gets, with the
  request's own over them. This gives a client no new power, since `agentctl exec` already runs any command in an
  agent.

Each attachment has its own tmux client, so tmux does what it does for several clients: it redraws the whole screen
for a new one and keeps each client's size.

| | How it travels |
| --- | --- |
| Input | An `input` frame is written to the Execution. Microsandbox applies backpressure there, so a fast paste waits rather than piles up. Input and output each wait on their own, so a program that takes no input while it draws still draws. |
| Resize | A `resize` frame resizes the Execution's terminal. The size in the request is the Execution's initial size, so the first screen is drawn at the right size. |
| Output | The Execution's output becomes `output` frames. `agentd` stops reading it while a write to a slow client waits, which pushes back on the Execution. A client that stops reading still gets detached when `agentd` stops. |
| Exit | The Execution's exit becomes `ended` with the exit code. A tmux client exits when someone detaches it with `Ctrl-b d` or when the session ends. |

## Detaching

A client detaches by sending `detach` or by closing the connection. Either way `agentd` asks the Execution to
terminate, which sends `SIGTERM`, and kills it if it is still running two seconds later.

For a session that is enough, because of how tmux handles signals. tmux's client answers `SIGTERM` while attached by
telling the server it is leaving and exiting. The server drops that client and keeps the session. It is the same as
`Ctrl-b d`, except that it needs no keys. tmux's client exits with code 1 after `SIGTERM`, and `agentd` reports that
as detached, since it asked for it. The real-tmux test in `tests/tmux_scrollback.mjs` checks this.

`detach` exists alongside closing because a client that keeps its terminal wants it put back. When tmux exits on
`SIGTERM` it writes the sequences that leave the alternate screen and turn off the modes it set. A client that sends
`detach` gets those as `output`, then `ended` with `{"detached": true}`. A client that just closes, such as a closed
browser tab or a killed `agentctl`, needs none of that.

For a terminal that is not a session's there is nothing to keep. An interactive shell ignores `SIGTERM`, so the kill
is what ends it.

## Several clients on one session

Each client has its own tmux client, so they see the same session at their own sizes. tmux's `window-size` option,
`latest` by default, sizes the window for the client that typed last. tmux's `session_attached` counts `agentd`'s
tmux clients like any other, so the idle timeout and the upgrade check work from it unchanged, and still see a person
who runs `tmux attach` over SSH.

## Limits

Terminals share two limits with everything else. `agentd` serves at most 128 connections and stops accepting more, so
a GUI with many tabs open could keep `agentctl` from reaching it at all. Microsandbox admits at most 128 Executions per
sandbox by default and refuses the next one, and `agentd`'s own Executions, which observe sessions and deliver prompts,
count against that too.

So `agentd` caps terminals itself, at 16 per agent and 64 in all, and refuses the next one with an error that names
the cap. Both leave room under the shared limits for the CLI and the reconcilers.

## When agentd stops

An attachment lives in `agentd`, so it ends when `agentd` does.

An upgrade refuses to start while a session is attached. The relay is not why. After an upgrade `agentd` relaunches
every quiescent session so that its harness runs with the new hooks, and that would restart the harness under the
person using it. `agentd` also refuses new terminals while it prepares for an upgrade, as it refuses other changes.

When `agentd` shuts down for another reason, it detaches every terminal so the sessions stay, and sends `ended` with
an error saying it is stopping. When it crashes, Microsandbox ends its Executions for it: the guest kills the process
group of every Execution whose host connection closed. tmux drops those clients, so no tmux client outlives `agentd`
to keep a session from idling. The tmux server runs in a session of its own, so the kill does not reach it.

A terminal that is not a session's ends with `agentd`, since its program is the Execution.

`agentctl attach` and the TUI attach again when an attachment ends because `agentd` stopped, once `agentd` answers
again. They wait 30 seconds for it, and then start it. tmux redraws the screen, so the person sees a flicker rather
than losing the session. Against an `agentd` from another build they say why instead.

## A client from another build

The Control API takes only a client of the same protocol and build (`DaemonInfo::require_compatible`), and the frames
are part of the protocol. Nothing negotiates frame types, because both ends always come from one build. A client
still running from before an upgrade finds the new `agentd` incompatible when it tries to attach again, and asks the
person to start it again.

## On Windows

The relay passes a Unix terminal's bytes as they are, so mouse reports, keys with modifiers and pastes reach the
program unchanged. On Windows it still turns crossterm's key events into the bytes a terminal sends, which covers
typing, pasting and resizing but not the mouse. The console's virtual terminal input mode would pass everything as it
is, and needs trying on a real console before it replaces that.

## What still opens the sandbox

A client still opens Microsandbox for what is not a terminal: `agentctl exec` without `-t`, the sign-in's read of its
credential file, and port forwards for `agentctl port-forward`, SSH and VNC. Each can move the same way, on a connection
its call upgrades: executions without a terminal with frames for standard input, output and error, then port forwards
as the bare byte stream. The Sandbox SDK keeps its own terminal attach, since a program that embeds the SDK and owns
its terminal still needs it.

## Costs

Every byte of terminal output passes through `agentd`, which runs on one thread with the reconcilers. Terminal traffic
is small, but a program that floods its terminal competes with reconciliation.
