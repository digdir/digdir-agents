# Terminals through agentd

A proposal, not yet built. Today every client that shows a terminal in an agent opens the agent's sandbox itself:
`agentctl attach`, `agentctl exec -it`, the TUI and the sandbox sign-in each read the sandbox's Provider assignment
from the Control API and then open Microsandbox under `agentd`'s home. This proposal has `agentd` run the terminal
instead and relay it to the client over the Control API, so a client needs nothing but the Control API socket.

## Why

- **A client should not need to know the sandbox.** To attach, a client today needs the Provider assignment that
  `sessions.v1.ensure` hands it in `AttachTarget`, the path of `agentd`'s home, and the Microsandbox Provider linked
  into its own binary. It then opens `agentd`'s Microsandbox state from a second process. The Control API is meant to
  be the whole contract between a client and `agentd`, and the two TODOs on `AttachTarget` and on
  `microsandbox::attach_terminal` say so.
- **Each client would have to support each Provider.** `sandbox::attach_terminal` and
  `sandbox::start_terminal_execution` match on the Provider and refuse any but Microsandbox. A second Provider would
  have to be built into every client, the web GUI's server among them.
- **Detaching depends on keys only a terminal can send.** Microsandbox's client-side attach watches the input for
  `ctrl-b,d` (`DETACH_KEYS`) to detach. A browser tab that closes, a process that gets killed, or a TUI that wants
  its screen back has no keys to send.
- **Every client writes the same relay.** The sandbox sign-in already relays a terminal by hand from the client, and
  the web GUI needs one too. With `agentd` relaying, they share one.

## How a terminal crosses the Control API

The Control API is JSON-RPC, one JSON object per line. A connection carries one request and one response at a time,
and the client opens a new connection for each call. Three ways to carry a byte stream over it were weighed.

**JSON-RPC messages on the connection**, with terminal output base64-encoded in notifications. The server has no
notifications today, and sends nothing on a connection except the response to its request. Base64 in JSON grows
every output byte by a third and must be decoded again, and terminal output is nearly all of the traffic. Polling in
the style of `resources.v1.watch`, one call per change, adds a round trip to every keystroke's echo.

**A second connection with a ticket.** A call returns a ticket, and the client presents it as the first line of a new
connection, which then carries the stream. The ticket protects nothing, since the socket is already readable and
writable only by the user, who could make the call that gets a ticket. It adds a round trip, a ticket store, an expiry
and the case of a ticket nobody redeems.

**The call upgrades its own connection** (proposed). The client calls `sessions.v1.attach`. `agentd` starts the
terminal and answers with an ordinary JSON-RPC response, and from then on the connection carries terminal frames in
both directions until it closes. An error, such as a session that is not running, is an ordinary JSON-RPC error and
the connection stays JSON. This fits what the code already does:

- The client already opens a connection per call, and keeps one open for as long as what it asked for lives.
  `Client::run` returns its connection as an `AgentLease`, and `agentd` deletes the ephemeral agent when that
  connection closes. An attachment that lives as long as its connection is the same idea.
- The server already serves each connection in its own loop (`serve_requests`), so handing the stream to a relay
  after one response changes that loop and nothing else.
- Closing the connection is the one thing every client does, however it ends, so it can be the detach.

Two details need care when building it. `Client::exchange` reads the response through a `BufReader` and returns
`into_inner()`, which drops anything already read past the response line. Frames can follow the response at once, so
the client must keep the reader. The server must likewise hand over its `BufReader`, not the bare stream.

### Frames

After the response, each frame is one type byte, a 32-bit big-endian payload length, and the payload. Output is
arbitrary bytes that can end in the middle of a UTF-8 character, so it travels as it is, unencoded. A frame's payload
is at most 64 KiB, and `agentd` splits larger output.

| Type | From | Payload | Meaning |
| --- | --- | --- | --- |
| `input` | client | bytes | Bytes typed or pasted, as the terminal sends them |
| `resize` | client | rows and columns, each a big-endian `u16` | The client's terminal changed size |
| `detach` | client | none | The client is leaving and wants the terminal reset first |
| `output` | `agentd` | bytes | What the program drew |
| `ended` | `agentd` | JSON: `{"exited": code}`, `{"detached": true}` or `{"error": message}` | The last frame before `agentd` closes the connection |

A plain byte stream, as `docker attach` uses with a TTY, would need side channels for the size and the exit status.
Docker uses a separate HTTP call for each. Framing keeps everything on the one connection. `agentd` never interprets
the output, which comes from the untrusted sandbox. It only cuts it into frames.

## What agentd runs

Two methods upgrade their connection:

- `sessions.v1.attach` with the agent, the session name and the client's terminal size. `agentd` starts a tmux client
  in the sandbox with the session's `attach_spec` as a terminal Execution (`SandboxHandle::start_terminal_execution`),
  at the client's size. The result is the `Session`, so a client can title its window.

  Attaching never creates a session. That stays with `sessions.v1.ensure`, which takes the harness, model and
  initial prompt, so a client that may create one calls it first. The GUI can then show a session it listed without
  carrying what creating one needs. Folding the two into one call would save a call but not change how progress is
  shown, since `Wait::until` follows an agent's progress alongside any call. Attaching does resume an Idle session and
  wait for a Resuming one, as `sessions.v1.prompt` does, so a session that went Idle between the two calls is no error.
- `agents.v1.terminal` with the agent, a command and the size, for terminals that are not a session's: the sign-in,
  `agentctl exec -it` and the TUI's shell. `agentd` builds the Execution with `platform::execution_spec`, as the client
  does today. This gives a client no new power, since `agentctl exec` already runs any command in an agent.

Each attachment gets its own Execution, so tmux does what it already does for several clients: it redraws the whole
screen for a new one, and keeps each client's size.

| | How it travels |
| --- | --- |
| Input | An `input` frame becomes `TerminalControl::write_input`. Microsandbox applies backpressure there, so a fast paste waits rather than piles up. |
| Resize | A `resize` frame becomes `TerminalControl::resize`. The kernel sends the tmux client `SIGWINCH`, and tmux redraws. The size in the request is the Execution's initial size, so the first screen is drawn at the right size. |
| Output | `TerminalEvent::Output` becomes `output` frames. `agentd` stops reading events while a write to a slow client waits, which pushes back on the Execution's stream. |
| Exit | `TerminalEvent::Exited` becomes `ended` with the exit code, and `TerminalEvent::Failed` becomes `ended` with the error. A tmux client exits when someone detaches it with `Ctrl-b d` or when the session ends. |

The runtime seam changes to match. `SessionRuntime::attach(home, target)`, the one method that runs in the client,
becomes a method that takes the `SandboxHandle` like every other one and returns the started terminal Execution for
`agentd` to relay.

## Detaching

A client detaches by sending `detach` or by closing the connection. Either way `agentd` asks the Execution to
terminate (`terminate_execution`, which sends `SIGTERM`), and kills it if it is still running after a short grace.

For a session that is enough, because of how tmux handles signals. tmux's client, in `client_signal` in `client.c`,
answers `SIGTERM` while attached by telling the server it is leaving and exiting. The server drops that client and
keeps the session. It is the same as `Ctrl-b d`, except that it needs no keys. Killing the client also leaves the
session, so the grace only matters for a clean exit. tmux's client exits with code 1 after `SIGTERM`, and `agentd`
reports that as detached, since it asked for it.

`agentd` addresses the tmux client by the Execution it started, not with `tmux detach-client`, which would need the
client's tty name from inside the guest.

`detach` exists alongside closing because a client that keeps its terminal wants it put back. When tmux exits on
`SIGTERM` it writes the sequences that leave the alternate screen and turn off mouse reporting and the other modes it
set. A client that sends `detach` gets those as `output`, then `ended` with `{"detached": true}`. A client that just
closes, such as a closed browser tab, needs none of that.

For a terminal that is not a session's there is nothing to keep. An interactive shell ignores `SIGTERM`, so the kill
after the grace is what ends it.

`DETACH_KEYS` and the detach keys in `AttachTerminalRequest` go. `Ctrl-b d` still works, because tmux reads it from the
input like any other key.

## What each client changes

The `agent` library's Control API client gets `Client::attach_session` and `Client::open_terminal`. Each returns a
terminal with input, resize and detach, and a stream of output that ends in how it ended. CLI clients share one
relay from that terminal to stdin and stdout.

- **`agentctl attach`** calls `sessions.v1.ensure` as now, with its progress, then `attach_session`. It puts the local
  terminal in raw mode and copies stdin bytes to `input` as they are, so mouse, extended keys, focus events and
  pastes reach tmux unchanged. It sends `resize` on `SIGWINCH`, and `detach` when it gets `SIGTERM` or `SIGHUP`
  itself. Today Microsandbox's client does all of this inside `attach_terminal`. The builder's attaches in `builder.rs`
  are the same call.
- **`agentctl exec -it`** uses `open_terminal` with the same relay.
- **The TUI** leaves its screen, runs the same relay for a session or for its shell, and takes its screen back after
  `ended`.
- **The sandbox sign-in** replaces `sandbox::start_terminal_execution` with `open_terminal`. It keeps the lease on its
  sign-in agent's own connection, and still scans the output for sign-in links, since the output passes through it.
  With the shared relay it can send stdin bytes as they are and drop `key_bytes`, which translates only the keys it
  lists.
- **The web GUI's server** opens one attachment per terminal the browser shows and relays it over a WebSocket: each
  frame becomes one binary WebSocket message with its type byte kept and its length dropped, since a WebSocket message
  has its own. It sends `resize` when xterm.js resizes and closes the attachment when the WebSocket closes. It never
  opens Microsandbox, and needs neither `agentd`'s home path nor the Provider linked in.

On Windows the relay has to put the console in virtual-terminal input mode, and convert between the console's UTF-16
and the UTF-8 the sandbox speaks. Microsandbox's attach does that today, in its SDK's `sandbox/terminal/` module, which
also handles a character split across two reads. agentctl gets a module of its own for it rather than calling
Microsandbox's, because the host's console is not a sandbox concern and the client should not need the Provider for
it. Microsandbox's code is Apache-2.0, so agentctl's is written with it as the reference rather than copied into the
MIT crate. The conversion is unit-tested in the Windows portable-host jobs in `rust.yml`, and attach is tried by
hand in Windows Terminal and in the classic console before it lands.

## What goes away

- `AttachTarget.sandbox`, and with it `AttachTarget`: `sessions.v1.ensure` returns the `Session`. `agentd` finds the
  sandbox itself when it serves `sessions.v1.attach`, as it does for `sessions.v1.prompt`.
- `sessions::attach`, and `attach_terminal` and `attach_request` in the tmux runtime.
- `agent::sandbox::attach_terminal` and `agent::sandbox::start_terminal_execution`, with their Microsandbox versions in
  `agentctl/src/sandbox/microsandbox/terminal.rs`.

The Sandbox SDK keeps `attach_terminal`, `AttachTerminalRequest` and `TerminalAttachOutcome`. The SDK stands without
agents, and a program that embeds it and owns its terminal, such as its `worktree` example, still needs them. Only
agentctl stops calling them.

The client still opens `home/microsandbox` for what is not a terminal: `agentctl exec` without `-t`, the sign-in's
read of its credential file (`sandbox::start_execution`), and port forwards for `agentctl forward`, SSH and VNC
(`guest_tcp_dialer`). Each moves in a later change, on a connection that its call upgrades:

1. Executions without a terminal, with frames for standard input, output and error and the exit status. The sign-in's
   credential read moves with them.
2. Port forwards, with a call that connects to a port in the sandbox and upgrades to the bare byte stream. A TCP
   stream needs no frames, since the client closing its write side stands for the end of its input.

Keeping them out of this change keeps it to what the GUI needs, and tries the upgraded connection on one kind of
stream first. Once both have moved, `ExecutionTarget.sandbox` goes and no client opens the sandbox. The runtime
diagram, which says terminals go straight to `msb`, changes with each.

## Several clients on one session

Each client has its own tmux client, so they see the same session at their own sizes. tmux's `window-size` option,
`latest` by default, sizes the window for the client that typed last. `parse_observation` reads tmux's
`session_attached` as a count of clients, so a session with several stays healthy.

Concurrent attaches also stop opening a Microsandbox Provider per attach. `agentd` already has one, so two clients
attaching at once share it.

`session_attached` counts `agentd`'s tmux clients like any other. The idle timeout and the upgrade check keep working
from it unchanged, and still see a person who runs `tmux attach` over SSH.

## Limits

Terminals share two limits with everything else. `agentd` serves at most 128 connections and stops accepting more,
so a GUI with many tabs open could keep `agentctl` from reaching it at all. Microsandbox admits at most
`max_executions` Executions per sandbox, 128 by default, and refuses the next one. `agentd`'s own Executions, which
observe sessions and deliver prompts, count against that too, and a session whose observation is refused goes Failed.

So `agentd` caps terminals itself, at 16 per agent and 64 in all, and refuses the next attachment with an error that
names the cap. Both leave room under the shared limits for the CLI and the reconcilers. They are starting points, to
revisit once the GUI shows how many terminals people keep open.

## When agentd stops

An attachment lives in `agentd` now, so it ends when `agentd` does, which a direct Microsandbox attach did not.

An upgrade keeps refusing to start while a session is attached, as it does today. The relay is not why. After an
upgrade `agentd` relaunches every quiescent session so that its harness runs with the new hooks
(`relaunch_after_upgrade`), and that would restart the harness under the person using it. `sessions.v1.attach` and
`agents.v1.terminal` are listed with the mutating methods, so `agentd` also refuses new attachments while it prepares
for an upgrade, as it refuses `sessions.v1.ensure`. A browser tab left open blocks an upgrade just as a terminal
window left attached does today, and the refusal names the session either way.

When `agentd` shuts down for another reason, it detaches every relay so the sessions stay, and sends `ended` with an
error saying it is stopping. When it crashes, Microsandbox ends its Executions for it: the guest agent kills the
process group of every Execution whose host connection closed (`cleanup_relay_client_range` in Microsandbox's guest
`agentd`). tmux drops those clients and `session_attached` falls back, so no tmux client outlives `agentd` to keep a
session from idling. The tmux server runs in a session of its own, so the kill does not reach it. This is what
already happens when `agentctl attach` is killed. A test that kills `agentd` during an attach confirms it.

A terminal that is not a session's ends with `agentd`, since its program is the Execution. A sign-in in progress
fails and is run again.

`agentctl attach`, the TUI and the GUI attach again when an attachment ends with an error or without `ended`, once
`health` reports a compatible `agentd`. tmux redraws the screen, so the person sees a flicker rather than losing the
session. Against an incompatible `agentd` they say why instead.

## A client from another build

The Control API takes only a client of the same protocol and build (`DaemonInfo::require_compatible`), and the frames
are part of the protocol. The change bumps `PROTOCOL_VERSION`, since it adds methods and changes the result of
`sessions.v1.ensure`. Nothing negotiates frame types, because both ends always come from one build. A client still
running from before an upgrade finds the new `agentd` incompatible when it tries to attach again, and asks the person
to start it again.

## Costs

Every byte of terminal output passes through `agentd`, which runs on one thread with the reconcilers. Terminal traffic
is small, but a program that floods its terminal competes with reconciliation. Bounded frames and Tokio's cooperative
budget should keep that fair, and it should be measured with a large `cat` before merging.

## For the maintainers to decide

Each of these is argued where the proposal makes it.

1. [An upgraded connection](#how-a-terminal-crosses-the-control-api) rather than a ticket for a second one, so a
   connection can leave JSON-RPC after its response.
2. [Attaching stays separate from ensuring](#what-agentd-runs) a session.
3. [Upgrades keep refusing](#when-agentd-stops) while a session is attached.
4. [Executions without a terminal and port forwards move later](#what-goes-away), in that order.
5. [The Sandbox SDK keeps its terminal attach](#what-goes-away).
6. [Terminals are capped](#limits) at 16 per agent and 64 in all.
7. [agentctl gets its own Windows console module](#what-each-client-changes) rather than using Microsandbox's.
