# Agent image contract

What `agentd` expects of the image an Agent runs. The [agentctl base images](../images/agentctl-base/README.md) meet
it, and an image built on them keeps meeting it as long as `agent-image-check` passes at the end of its build. `agentd`
reads no image labels. It probes most requirements below inside the running Sandbox and fails Agent setup with the
step that needs them; others, such as the home directory's ownership, only show as failures later.

## Always

- **The user.** Sessions, Executions and SSH logins run as the image's `USER`, which must be `agent`, with group
  `agent`, home `/home/agent` and passwordless `sudo`: setup runs its privileged steps through `/usr/bin/sudo`, some
  with `-n`. `agentd` creates `/home/agent/code`, the working directory, if it is missing. Keep everything under
  `/home/agent` owned by `agent`: a root-owned cache there locks the Agent out of it.
- **Executables at fixed paths.** `env`, `sudo`, `tmux`, `tar`, `install`, `mkdir`, `chown`, `chmod`, `test`, `find`,
  `grep`, `stat`, `date` and `tail` under `/usr/bin`, and `sh`, `sleep`, `rm`, `cat`, `chmod` and `chown` under `/bin`,
  as a merged `/usr` provides them. Sessions run in tmux.
- **Locale and terminal.** Every process runs with `LANG=C.UTF-8` and `TERM=xterm-256color`; glibc's built-in `C.UTF-8`
  and ncurses-base's terminfo provide them.
- **Trust.** `/etc/ssl/certs/ca-certificates.crt` must exist, from the `ca-certificates` package; the Sandbox adds the
  mediated CA to it.
- **Harnesses.** `agentd` never installs a harness. Each installation an Agent declares must be on the `PATH` of the
  image's environment as `claude`, `codex` or `pi`, and is probed with `<name> --version`. A declared `version` must
  match exactly. `optional: true` only omits an installation while the host has no login for it; once there is one,
  the binary must be in the image. Pin the versions tested by [HARNESSES.md](HARNESSES.md).
- **Node.js.** The Claude Code and Codex activity hooks and Claude Code's status line run as `node <script>`, so `node`
  must be on `PATH` for those harnesses.

## When present

- **git.** With `git` on `PATH`, `agentd` sets the global `user.name` and `user.email` from `GIT_USER_NAME` and
  `GIT_USER_EMAIL`. Without it, the step is skipped.
- **Podman.** With `/usr/bin/podman`, `agentd` waits for systemd, configures Podman to trust the mediated CA and
  enables `podman.socket` for the `agent` group, which every Execution reaches through
  `CONTAINER_HOST=unix:///run/podman/podman.sock`. An image with Podman therefore needs systemd running, and the
  `podman.socket` unit.

## Access

- **`ssh`** needs `initSystem: image` with systemd as the entrypoint, `/usr/sbin/sshd` and `/usr/bin/systemctl`.
  `agentd` writes the server's keys, policy and unit itself and listens only on loopback; the image must not run its
  own server, so mask `ssh.service` and `ssh.socket` and remove the package's host keys. The `agent` account must not
  be locked (`passwd --delete agent`), since `sshd` rejects a locked account even for key authentication when PAM is
  disabled.
- **`vnc`** needs systemd as the entrypoint, as `ssh` does, and `/etc/agent-access.d/vnc.conf`, a `key=value` file
  naming the units that grant access and the ports they serve:

  ```ini
  units=agent-vnc.socket agent-vnc-web.service
  port=5900
  web-port=6080
  ```

  `units` lists one to eight `.socket` or `.service` units, shipped disabled, which `agentd` enables with
  `systemctl enable --now` to grant access and disables to withdraw it. `port` must be 5900 and `web-port`, which is
  optional, 6080. `agentd` checks with `/usr/bin/ss` that the ports listen.

## What `agentd` writes

An image does not need to provide what `agentd` writes on every pass: the harnesses' credential placeholders, hooks,
settings, instructions and skills in their own configuration directories, the SSH server state, Podman's CA
configuration, the git identity and `/etc/agent-platform-api-url`. It leaves other files in those directories alone,
so an image may ship skills of its own there, as the desktop base image does. It does not write Claude Code's
onboarding and trust state in `~/.claude/.claude.json`; an image that runs Claude Code ships that.
