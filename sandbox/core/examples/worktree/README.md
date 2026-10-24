# Worktree sandbox

Runs Codex or Claude Code in a Sandbox through the Sandbox SDK alone, without `agentd`, with the current Git worktree
and your Codex and Claude Code configuration mounted from your home directory. The
[self-dev Agent](../../../../agents/self-dev/README.md) does the same through `agentd`, with fresh clones instead of a
mounted worktree.

Its [Dockerfile](Dockerfile) installs common toolchains and container tooling. The Sandbox protects the host
filesystem, but its network access is not restricted.

Run from the repository root:

```sh
cargo run -p sandbox-worktree                              # Start Codex
cargo run -p sandbox-worktree -- --harness claude          # Start Claude Code
cargo run -p sandbox-worktree -- delete                    # Delete the Sandbox
cargo run -p sandbox-worktree -- --name my-sandbox         # Override the worktree-derived name
```
