# Sandbox SDK

Rust crates for creating and running sandboxes, isolated computers for untrusted work, independent of agents.
`agentctl` builds on them, and other projects can pin a revision of this repository to use them too.

| Crate | What it is |
| --- | --- |
| [`sandbox`](core/) | The SDK: the contracts a Provider implements and the types they share |
| [`sandbox-microsandbox`](microsandbox/) | The Provider on Digdir's [Microsandbox](https://github.com/digdir/microsandbox) fork |
| [`sandbox-authorization`](authorization/) | The contracts and vocabulary for deciding what a sandbox may reach |

[core/examples/worktree](core/examples/worktree/) runs a harness in a sandbox without `agentd`.
[docs/](docs/README.md) lists its docs, such as how to update the Microsandbox pin.
