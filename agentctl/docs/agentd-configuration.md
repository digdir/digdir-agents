# agentd configuration

`agentd` reads its settings from `agentd.toml` in its home, `~/.agent` unless `AGENT_HOME` names another. Without the
file, or without a setting in it, `agentd` uses the default. `agentd` does not start with a file it cannot read, and
says why in `agentd.log` in its home, which `agentctl` points to. A file it cannot read while it runs leaves the last
settings it could read in place, and the log says why.

```toml
lowDiskThreshold = "5%"
```

## lowDiskThreshold

How little free space on the host filesystem holding the agents' disks makes `agentd` warn. Every agent's disk lives
there and grows as the agent writes, so they all fill it together. That filesystem is the one holding `agentd`'s home,
unless Microsandbox's `config.json` in `microsandbox/runtime` in the home moves the disks elsewhere with
`paths.sandboxes`. Below the threshold, every agent has the condition `HostDiskLow=True`, whose message says how much
is free. `agentctl get`, `agentctl describe` and `agentctl tui` show it until there is enough space again. The status
line of `agentctl tui` shows the free space there once `agentd` has measured it, and turns yellow below the
threshold.

While the filesystem is below the threshold, `agentd` also refuses to make or start an agent, since a new agent's disk
grows into the space the running ones need. That is `agentctl apply` of a new agent, or of a stopped one whose manifest
sets `runState: Running`, `agentctl start` and `agentctl run`, and the error says how much is free. Pass
`--allow-low-disk` to one of them to start that agent anyway. Applying a stopped agent without `runState` keeps it
stopped, so it is not refused. `agentd` still restarts an agent that is already running, since its disk already holds
its space.

`agentd` measures the free space again for each of these requests, so the answer does not wait for the next regular
check. When it cannot measure it within a few seconds, it starts the agent anyway and says why in `agentd.log`, so that
a fault in measuring does not keep every agent from starting.

Give a share of the filesystem, such as `"5%"` or `"2.5%"`, or a size, as a manifest gives one, such as `"20Gi"`. The
default is `"5%"`.

`agentd` checks the free space regularly and reads this setting again each time, so a change applies within a minute,
without restarting `agentd`.
