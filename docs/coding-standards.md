# Coding standards

How to write code here, beyond what `make fmt lint` enforces. Where code belongs, who owns what and how components
talk to each other is in the [architecture principles](architecture.md#principles).

## Design

- Do not add abstractions ahead of a concrete need, such as traits or channels for implementations that do not exist
  yet.
- Make invalid states unrepresentable. Parse input once, where it enters, into types that cannot hold an invalid
  value, so the code past that point never checks it again.
- Give domain values their own types, such as `SandboxName` or `ByteQuantity`, instead of passing plain strings and
  numbers around.
- When a rule can be checked, check it in a test, as `agentctl/tests/architecture.rs` does for adapter boundaries,
  instead of only writing it down.

## Rust

- Use Tokio's `LocalRuntime` and keep state single-threaded with `Rc`, `Cell` and `RefCell`. Do not use `Arc` or
  `Mutex` unless a dependency's API requires them.

## Tests

A test checks behavior that could break. Avoid tests that cannot fail or only restate the code:

- Do not assert what the test just set up, such as a constant's value, a struct round-tripping its own fields or a
  mock returning what it was told to.
- A crate's tests do not depend on files outside it. `agentctl` tests do not read the manifests in `agents/`. A test
  that needs one creates it as a fixture.
- Do not assert on incidental details, such as the full text of an error message or log line, when the behavior is
  what matters.
- Generally prefer testing from a high level, independent of implementation details.
