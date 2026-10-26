# Glossary

The terms are ordinary nouns, written in lowercase like any other word, except at the start of a sentence and in
identifiers and code, such as `kind: Agent` or a Rust type.

| Term | Meaning |
| --- | --- |
| Agent | A computer that runs coding work for you: what its manifest declares, the sandbox that runs it and the sessions in it. It lives on until you delete it. |
| Agent definition | The files that define an agent, kept together in one directory: its manifest and variants, and what they refer to, such as a Dockerfile, instructions, skills and a home overlay. |
| Agent manifest | The `agent.yaml` file that declares an agent. |
| Agent variant | An agent manifest, `agent.<variant>.yaml`, that extends another one beside it and changes only what differs. |
| `agentctl` | The command-line tool and TUI for agents. It talks to `agentd` and holds no state of its own. |
| `agentd` | The daemon on the host that owns agents: it stores what each manifest declares and keeps the sandbox and sessions matching it. Not to be confused with Microsandbox's guest agent, which also runs as `agentd` inside the sandbox. |
| Control API | How `agentctl`, or any other client, talks to `agentd`. Local to the host. |
| Execution | A command run in a sandbox from the host, such as `agentctl exec` or a setup step. |
| Harness | The program that runs a model in a loop with tools, such as Claude Code, Codex or Pi. A session runs one. |
| Harness adapter | The part of `agentd` that knows one harness: how it logs in, launches, reports activity and resumes. |
| Host | The machine `agentctl` and `agentd` run on. Trusted: it holds the real secrets and decides what the sandbox may reach. |
| Image | The container image an agent's sandbox boots from. Usually built on the agentctl base images. |
| Interface | How a person reaches an agent: `agentctl attach` or the TUI, SSH, VNC, an editor, or a desktop app connected over SSH. An interface talks to a harness, which talks to a model. |
| Mediated network | All sandbox traffic ends at the host, which applies the agent's policy and puts real secrets into requests to the hosts allowed for them. |
| Nested agent | An agent started from inside another agent. |
| Placeholder | What the sandbox sees instead of a secret's real value. |
| Platform API | The one host endpoint the sandbox may reach. The harnesses report activity through it. |
| Provider | An implementation of the Sandbox SDK's contracts, such as Microsandbox. |
| Reconcile | To compare an agent or session with what is declared and act to close the gap, again and again until they match. |
| Sandbox | An isolated computer for untrusted work. It has its own filesystem, processes and network, and reaches the host and the outside world only through what the host allows. Nothing the host relies on runs in it. |
| Sandbox SDK | The crates in `sandbox/` for creating and running sandboxes, independent of agents. |
| Secret | A value the host holds for an agent, such as a token, that never enters the sandbox. |
| Session | One harness running in an agent, durable and resumable. An agent can have several. |
