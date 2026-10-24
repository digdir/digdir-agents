# Glossary

| Term | Meaning |
| --- | --- |
| Agent | A computer that runs coding work for you: what its manifest declares, the Sandbox that runs it and the Sessions in it. It lives on until you delete it. |
| `agentctl` | The command-line tool and TUI for Agents. It talks to `agentd` and holds no state of its own. |
| `agentd` | The daemon on the host that owns Agents: it stores what each manifest declares and keeps the Sandbox and Sessions matching it. Not to be confused with Microsandbox's guest agent, which also runs as `agentd` inside the Sandbox. |
| Control API | How `agentctl`, or any other client, talks to `agentd`. Local to the host. |
| Execution | A command run in a Sandbox from the host, such as `agentctl exec` or a setup step. |
| Harness | The program that runs a model in a loop with tools, such as Claude Code, Codex or Pi. A Session runs one. |
| Harness adapter | The part of `agentd` that knows one harness: how it logs in, launches, reports activity and resumes. |
| Host | The machine `agentctl` and `agentd` run on. Trusted: it holds the real secrets and decides what the Sandbox may reach. |
| Image | The container image an Agent's Sandbox boots from. Usually built on the agentctl base images. |
| Interface | How a person reaches an Agent: `agentctl attach` or the TUI, SSH, VNC, an editor, or a desktop app connected over SSH. An interface talks to a harness, which talks to a model. |
| Manifest | The `agent.yaml` file that declares an Agent. |
| Variant | A manifest, `agent.<variant>.yaml`, that extends another manifest beside it and changes only what differs. |
| Mediated network | All Sandbox traffic ends at the host, which applies the Agent's policy and puts real secrets into requests to the hosts allowed for them. |
| Nested Agent | An Agent started from inside another Agent. |
| Placeholder | What the Sandbox sees instead of a secret's real value. |
| Platform API | The one host endpoint the Sandbox may reach. The harnesses report activity through it. |
| Provider | An implementation of the Sandbox SDK's contracts, such as Microsandbox. |
| Reconcile | To compare an Agent or Session with what is declared and act to close the gap, again and again until they match. |
| Sandbox | An isolated computer for untrusted work. It has its own filesystem, processes and network, and reaches the host and the outside world only through what the host allows. Nothing the host relies on runs in it. |
| Sandbox SDK | The crates in `sandbox/` for creating and running Sandboxes, independent of Agents. |
| Secret | A value the host holds for an Agent, such as a token, that never enters the Sandbox. |
| Session | One harness running in an Agent, durable and resumable. An Agent can have several. |
