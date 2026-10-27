# Changelog

All notable changes to the agent platform will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Entries should describe only user-facing functionality in clear, user-friendly language; omit implementation details that do not affect how people use the product.
Section ordering: Added, Changed, Fixed, Removed, Security, Deprecated.

The version is the agentctl release published under the `agentctl/v*` tag, covering `agentctl`, `agentd` and the example
agents. The Rust workspace version is a build detail and is not tracked here.

## [Unreleased]

### Added

- `optional: true` in `spec.instructions` and `spec.skills` leaves out a source that does not exist, so a committed manifest can name your own files, such as `~/agent-instructions/everywhere.md`. `agentctl describe` names what was left out, and the agent gets it once it exists. ([#331](https://github.com/digdir/digdir-agents/issues/331))
- Your terminal tab is named after the agent and session you are attached to, such as `builder/b1`, and reads `agentctl` again when you return to `agentctl tui`. ([#343](https://github.com/digdir/digdir-agents/pull/343))
- `agentctl get agents --watch -o json` prints the Agents as one JSON line, then a new line each time they change, so a tool can follow them without polling. It reconnects when `agentd` restarts and exits once an upgrade makes it incompatible. `get sessions --watch` works the same way. ([#349](https://github.com/digdir/digdir-agents/issues/349))

## [0.1.0-preview.13] - 2026-10-26

### Added

- Agents' instructions now start with a short Agent sandbox section from `agentd`, listing the Agent's CPUs, memory and disk, harnesses, secrets and their hosts, network, access, and whether nested Agents can run. An instruction file from the image or `spec.home` keeps its text below it. Every Agent also gets an `agentctl` skill for driving Agents, including nested ones. ([#284](https://github.com/digdir/digdir-agents/pull/284))
- Agent images that run nested Agents can copy `agentctl` and `agentd` from `ghcr.io/digdir/agentctl`, tagged by release, `main` or `sha-<commit>`, instead of building them. ([agentctl image](https://github.com/digdir/digdir-agents/tree/main/images/agentctl), [#282](https://github.com/digdir/digdir-agents/pull/282))
- `spec.sandbox.resources.nestedSandboxes` lets an Agent run Sandboxes of its own, such as nested Agents, on Linux x86_64 with nested KVM or an Apple M3 or newer with macOS 15 or newer. `required` fails on other hosts, and `preferred` runs without them there. Images no longer need to let the `agent` user open `/dev/kvm`. ([#231](https://github.com/digdir/digdir-agents/pull/231))
- Sessions know where they run: `AGENT_NAME`, `AGENT_SESSION_NAME`, `AGENT_HOST`, `AGENT_SOURCE_DIRECTORY` and `AGENT_HARNESS` are set for the harness, so instructions can ask an agent to name itself and tools can suit its model. ([#206](https://github.com/digdir/digdir-agents/pull/206), [#263](https://github.com/digdir/digdir-agents/pull/263))
- Images built on the base images can add Podman, the `docker` command and Docker Compose, usable by the agent user from boot, with `agent-feature install containers`. ([features](https://github.com/digdir/digdir-agents/tree/main/images/agentctl-base#features), [#266](https://github.com/digdir/digdir-agents/pull/266))
- The tmux status bar in a session shows its name and the keys to detach, open a shell beside it and move to the next window. ([#206](https://github.com/digdir/digdir-agents/pull/206))
- On the desktop image, apps that keep secrets in a keyring, such as `secret-tool`, Chromium and VS Code, now have one. It has no password: anything in the Agent can read it. ([Secret Service](https://github.com/digdir/digdir-agents/tree/main/images/agentctl-base#the-secret-service), [#198](https://github.com/digdir/digdir-agents/pull/198))
- `agentctl kill --all`, or `agentctl stop --all --now`, is an emergency stop: every Agent's VM ends at once, without shutting down, and stays stopped until started again. `agentctl stop` without an Agent asks before stopping every running one, and `X` in `agentctl tui` stops them all at once. ([#205](https://github.com/digdir/digdir-agents/pull/205))
- `agentctl restart agent/NAME`, or `r` in `agentctl tui`, restarts an Agent's VM on the same disk and waits until it is Ready, also when it stopped responding. Its Sessions resume when attached. `--now` ends the VM without letting it shut down. ([#226](https://github.com/digdir/digdir-agents/issues/226))
- Images built on the desktop image can override its settings, such as `AGENT_DESKTOP_KEYBOARD`, in `/etc/agent-desktop.conf.d/`. ([desktop settings](https://github.com/digdir/digdir-agents/tree/main/images/agentctl-base#desktop-settings), [#262](https://github.com/digdir/digdir-agents/pull/262))
- On the base images, a repository pinned to another Playwright installs its own Chromium build with `npx playwright install chromium`, without sudo. ([browser](https://github.com/digdir/digdir-agents/tree/main/images/agentctl-base#the-browser), [#265](https://github.com/digdir/digdir-agents/pull/265))
- Images built on the base images can install another Node.js, such as Node.js 26, as the README describes. ([Using another Node.js](https://github.com/digdir/digdir-agents/tree/main/images/agentctl-base#using-another-nodejs), [#265](https://github.com/digdir/digdir-agents/pull/265))
- The base images include `shellcheck`, `actionlint`, `make`, `patch`, `file`, `unzip`, `zip` and `wget`, so an Agent's Dockerfile need not install them. ([images](https://github.com/digdir/digdir-agents/tree/main/images/agentctl-base#building-on-them), [#285](https://github.com/digdir/digdir-agents/pull/285))
- Host paths in a manifest may start with `~/` for your home directory, so a manifest can name your own skills, such as `~/.claude/skills/review`, wherever it is applied. ([#250](https://github.com/digdir/digdir-agents/pull/250))

### Changed

- Inside an Agent, `agentctl claude login` and `agentctl codex login` reuse that Agent's own login, or say what is missing, so a nested Agent needs no `--from-stdin`. An Agent waiting for a login installs the harness within seconds, restarting its VM. ([#284](https://github.com/digdir/digdir-agents/pull/284))
- Breaking: the base images no longer set `NODE_PATH`, so a repository that uses a package missing from its `package.json`, such as `playwright` or `ws`, now fails to load it. Run scripts outside a project with `NODE_PATH="$(npm root -g)" node script.js`, and remove an `ENV NODE_PATH` an image copied from the README. ([browser](https://github.com/digdir/digdir-agents/tree/main/images/agentctl-base#the-browser), [#265](https://github.com/digdir/digdir-agents/pull/265))
- Breaking: on Linux x86_64, Agents no longer have `/dev/kvm` unless their manifest sets `spec.sandbox.resources.nestedSandboxes`. Add it to an Agent that runs virtual machines and apply the manifest again, which restarts the Agent. ([#231](https://github.com/digdir/digdir-agents/pull/231))
- Breaking: images that run `agent-feature install desktop` need `ENV XDG_CURRENT_DESKTOP=X-Openbox:XFCE`, which `agent-feature env desktop` prints; `agent-image-check` fails until it is added. Images built on the desktop image inherit it. ([features](https://github.com/digdir/digdir-agents/tree/main/images/agentctl-base#features), [#198](https://github.com/digdir/digdir-agents/pull/198))
- VNC clients can no longer resize an Agent's desktop to fit their window; set them to scale instead. ([#198](https://github.com/digdir/digdir-agents/pull/198))
- Newly created Agents using the shared images ship Node.js 24 LTS (24.22.1) and updated tools, including GitHub CLI 2.103.0, Claude Code 2.1.309 and Codex CLI 0.164.0. ([#198](https://github.com/digdir/digdir-agents/pull/198), [#229](https://github.com/digdir/digdir-agents/pull/229))
- Breaking: the desktop image's keyboard layout is now US rather than Norwegian. An image that needs Norwegian sets `AGENT_DESKTOP_KEYBOARD=no` in `/etc/agent-desktop.conf.d/`. `desktop display` shows the layout. ([desktop settings](https://github.com/digdir/digdir-agents/tree/main/images/agentctl-base#desktop-settings), [#262](https://github.com/digdir/digdir-agents/pull/262))
- The desktop image's display is now 1600x900, and `desktop screenshot` sizes screenshots for the Session's harness: Claude Code gets 1600x900 at 1445x812, Codex at full size. To keep 1456x819, set `AGENT_DESKTOP_GEOMETRY` in `/etc/agent-desktop.conf.d/`. ([screenshot budget](https://github.com/digdir/digdir-agents/tree/main/images/agentctl-base#the-screenshot-budget), [#263](https://github.com/digdir/digdir-agents/pull/263))
- In an Agent, your own Claude Code settings can no longer change how it handles Bash prompts in bypass permissions mode, how long it keeps conversations, or the setting that lets it use Fable. ([#292](https://github.com/digdir/digdir-agents/pull/292))

### Fixed

- Large builds in new Agents built on the base images can use the available disk space in `/tmp`. Existing Agents need an updated image, removal of any `/tmp` tmpfs mount, and recreation after saving work. ([temporary files](https://github.com/digdir/digdir-agents/tree/main/images/agentctl-base#temporary-files), [#329](https://github.com/digdir/digdir-agents/pull/329))
- `agentctl tui` no longer searches every folder below the current one for manifests to create an Agent from when it is not run inside a git repository, which was slow from a home directory and offered manifests from every checkout. Outside a repository it offers `agent.yaml` and `agents/*/agent.yaml` in the current folder, with their variants. ([#283](https://github.com/digdir/digdir-agents/pull/283))
- `agentctl claude login` no longer appears to hang: ([#270](https://github.com/digdir/digdir-agents/pull/270))
  - under WSL, it opens the sign-in page in the Windows browser, as `agentctl` now does for every address
  - where no browser would reach you, such as over SSH, it shows a link to open on any device and asks you to paste the token. `AGENTCTL_OPEN=copy` asks anywhere
  - a browser sign-in that does not complete within 5 minutes stops
- In Agents with Podman, `podman` and `docker` run by the agent user use the Agent's shared Podman everywhere, such as in systemd units and cron, instead of failing outside Sessions, `agentctl exec` and SSH logins. Those no longer set `CONTAINER_HOST`. ([#266](https://github.com/digdir/digdir-agents/pull/266))
- Node.js programs in an Agent, such as `npm`, `corepack` and Pi, no longer fail with `self-signed certificate in certificate chain` in Sessions, `agentctl exec` and SSH logins. Restart a running Agent with `agentctl stop` and `agentctl start` to fix its Sessions. ([#235](https://github.com/digdir/digdir-agents/issues/235))
- In Agents, Claude Code in bypass permissions mode no longer stops to ask before running Bash commands such as `rm`, whether in a Session, T3 Code or `agentctl ssh`. Other prompts still appear, and so do Bash prompts in modes that ask for approval. ([#237](https://github.com/digdir/digdir-agents/pull/237), [#292](https://github.com/digdir/digdir-agents/pull/292))
- Claude Code in an Agent no longer deletes conversations after 30 days. Before, running it outside a Session, such as in T3 Code, deleted old conversations, including those of Idle Sessions. ([#292](https://github.com/digdir/digdir-agents/pull/292))
- Fable works without a usage-credits prompt everywhere in an Agent, not only in Sessions, such as in T3 Code. ([#292](https://github.com/digdir/digdir-agents/pull/292))
- Claude Sessions no longer hang on the startup bypass-permissions warning when starting or resuming in Agents with custom user settings in `spec.home`. ([#230](https://github.com/digdir/digdir-agents/pull/230))
- `agentctl exec` without `-it` no longer waits forever on commands that read standard input, such as `cat`; they get an empty input and finish. ([#231](https://github.com/digdir/digdir-agents/pull/231))
- `agentctl apply` no longer fails with `Socket not found: /var/run/docker.sock` for Dockerfile-backed Agents using Docker Desktop or Colima's selected local context. Background retries recover after starting Docker or switching local contexts without restarting `agentd`. ([#220](https://github.com/digdir/digdir-agents/pull/220))
- On macOS and Linux, named Sandbox volumes can mount when the Agent data directory is reached through a symbolic link, including custom `AGENT_HOME` locations. ([#221](https://github.com/digdir/digdir-agents/pull/221))
- On the base images, a build that runs out of memory in a Session, over SSH or in a desktop terminal no longer takes the Agent down: it keeps responding, and other SSH connections and desktop windows stay open. ([#198](https://github.com/digdir/digdir-agents/pull/198))
- On the desktop image, `desktop` clicks no longer land somewhere other than the last screenshot showed after the display changes size; `desktop` asks for a new screenshot instead. ([#198](https://github.com/digdir/digdir-agents/pull/198))
- Repositories the base images clone at boot from `/etc/agent-workspace.d`: ([base images](https://github.com/digdir/digdir-agents/tree/main/images/agentctl-base#building-on-them), [#198](https://github.com/digdir/digdir-agents/pull/198))
  - an interrupted clone is made again at the next boot, instead of staying broken
  - when a listed repository is a fork, `gh pr create` and `gh issue` target it rather than its parent
- `agentctl turns` and the recent turns panel in `agentctl tui` no longer fail with "the last 3 complete turns do not fit within the 2 MiB transcript read limit" when the latest turns hold large tool output, such as screenshots. Turns are now read from the last 64 MiB of a Session's conversation. ([#199](https://github.com/digdir/digdir-agents/pull/199))
- On the desktop image, `desktop type` enters capital letters the keyboard layout lacks, such as `ÆØÅ` under a US layout or `Ü` under a Norwegian one, instead of their lower case. ([#262](https://github.com/digdir/digdir-agents/pull/262))

## [0.1.0-preview.12] - 2026-10-20

### Added

- Agent images can build on `ghcr.io/digdir/agentctl-base:ubuntu-26.04`, or `ubuntu-26.04-desktop` for Chromium and a VNC desktop, which ship Claude Code, Codex and Pi at the tested versions. `agent-feature install browser` or `desktop` adds those parts to an image's own stage, and `agent-image-check` reports what an image still lacks. ([base images](https://github.com/digdir/digdir-agents/tree/main/images/agentctl-base), [image contract](https://github.com/digdir/digdir-agents/blob/main/agentctl/docs/image-contract.md), [#187](https://github.com/digdir/digdir-agents/pull/187))
- Agents can offer Pi (`type: pi`) as a third harness, configured entirely by the Agent: its providers, models and keys come from its own `~/.pi/agent/` files and `spec.secrets`. Adding Pi to an existing Agent means deleting and re-applying it. ([self-dev Agent](https://github.com/digdir/digdir-agents/tree/main/agents/self-dev#pi), [#182](https://github.com/digdir/digdir-agents/pull/182))

### Changed

- Sessions are tested with Claude Code 2.1.302 and Codex CLI 0.163.0, so Agents with their own images can install these versions. ([#181](https://github.com/digdir/digdir-agents/pull/181))
- Session terminals encode modified keys as CSI u on tmux 3.5 and later, for every harness, so Shift+Enter inserts a newline in Codex. ([#182](https://github.com/digdir/digdir-agents/pull/182))
- `spec.harnesses[].auth` may be omitted; it follows from the harness. ([#182](https://github.com/digdir/digdir-agents/pull/182))

### Security

- `agentctl` and `agentd` use rustls 0.23.46. They were not affected by the advisories it fixes, so a scanner that flags rustls 0.23.45 in 0.1.0-preview.11 needs no urgent upgrade ([RUSTSEC-2026-0346](https://rustsec.org/advisories/RUSTSEC-2026-0346), [-0347](https://rustsec.org/advisories/RUSTSEC-2026-0347), [-0348](https://rustsec.org/advisories/RUSTSEC-2026-0348), [-0349](https://rustsec.org/advisories/RUSTSEC-2026-0349), [#177](https://github.com/digdir/digdir-agents/pull/177)).
- `agentctl` and `agentd` reject TLS certificates that chain through the distrusted emSign intermediates, whose private keys are now public; 0.1.0-preview.11 and earlier may accept them ([RUSTSEC-2026-0382](https://rustsec.org/advisories/RUSTSEC-2026-0382), [RUSTSEC-2026-0381](https://rustsec.org/advisories/RUSTSEC-2026-0381), [#196](https://github.com/digdir/digdir-agents/pull/196)).

## [0.1.0-preview.11] - 2026-10-12

### Changed

- `agentctl tui` suggests a numbered name when it creates an Agent, such as `agentctl-dev-1`, skipping names already in use; type a name to use another. ([#158](https://github.com/digdir/digdir-agents/pull/158))
- Sessions are tested with Claude Code 2.1.296 and Codex CLI 0.162.1, so Agents with their own images can install these versions. ([#154](https://github.com/digdir/digdir-agents/pull/154), [#168](https://github.com/digdir/digdir-agents/pull/168))

### Fixed

- On Linux and macOS, installing with `install.sh` no longer fails with "curl: (22) The requested URL returned error: 422", or 403 on later attempts, while it looks up the latest release. ([install](https://github.com/digdir/digdir-agents/tree/main/agentctl#install), [#170](https://github.com/digdir/digdir-agents/pull/170))
- `agentctl` commands, such as `agentctl tui`, no longer fail after a reboot with "automatic startup exited with exit code: 1" when Windows or another program has reserved the port `agentd` used before. ([#160](https://github.com/digdir/digdir-agents/issues/160))
- Windows installation and updates tolerate temporary file locks while publishing a release. ([#152](https://github.com/digdir/digdir-agents/issues/152))
- Completed updates no longer block commands or installation when old releases have been removed, and reinstalling repairs a current release pointer that names a missing release. ([#153](https://github.com/digdir/digdir-agents/issues/153))

## [0.1.0-preview.10] - 2026-10-06

### Added

- The self-development Agent:
  - clones `digdir/microsandbox` and `digdir/libkrunfw` and has the `changelog` skill ([#136](https://github.com/digdir/digdir-agents/pull/136))
  - uses the published image `ghcr.io/digdir/digdir-agents/agent-self-dev`; its new `nested-build` variant builds the image from the checkout ([#138](https://github.com/digdir/digdir-agents/pull/138))
  - includes more development tools, such as `cargo-deny`, `shellcheck`, `actionlint`, `sqlite3` and `hyperfine` ([#138](https://github.com/digdir/digdir-agents/pull/138))

### Changed

- Breaking: `agentctl` is now developed and released in [digdir/digdir-agents](https://github.com/digdir/digdir-agents). Rerun the [installer](https://github.com/digdir/digdir-agents/tree/main/agentctl#install) once; Agents and Sessions carry over, and `agentctl self update` then follows the new releases. ([#136](https://github.com/digdir/digdir-agents/pull/136))
- The self-development Agent is created without Codex when the host has no Codex login; `agentctl codex login` adds it later. ([#138](https://github.com/digdir/digdir-agents/pull/138))

## [0.1.0-preview.9] - 2026-10-02

### Added

- `agentctl tui` opens an Agent with `o`:
  - in a shell, VS Code, Zed or SSH, offering to add the `Include` to `~/.ssh/config` first ([#20762](https://github.com/Altinn/altinn-studio/pull/20762))
  - on its desktop, in the browser or a VNC client ([#20763](https://github.com/Altinn/altinn-studio/pull/20763))
  - through a forward, from the forwards view ([#20763](https://github.com/Altinn/altinn-studio/pull/20763))
- `agentctl ssh-info` reports the directory editors open, as `workingDirectory` in JSON. ([#20762](https://github.com/Altinn/altinn-studio/pull/20762))
- `agentctl stop` and `agentctl start`, or `x` in `agentctl tui`, stop an Agent's VM and start it again on the same disk, also one that stopped responding. Its Sessions go Idle and resume when attached after the start, and re-applying keeps a stopped Agent stopped. ([#20807](https://github.com/Altinn/altinn-studio/issues/20807))

### Changed

- Altinn, self-development, minimal and worktree Agents install Claude Code 2.1.286. ([#20886](https://github.com/Altinn/altinn-studio/pull/20886))
- Altinn, self-development and worktree Agents install Codex CLI 0.159.3. ([#20886](https://github.com/Altinn/altinn-studio/pull/20886))
- `agentctl tui` port forwards: ([#20763](https://github.com/Altinn/altinn-studio/pull/20763))
  - `q` asks before quitting would close them
  - they close when their Agent is deleted or re-created

### Fixed

- Windows Agents remain responsive after startup, so commands, SSH and Sessions keep working. Restart running Agents to apply the runtime update. ([#20911](https://github.com/Altinn/altinn-studio/pull/20911), [martinothamar/microsandbox#9](https://github.com/martinothamar/microsandbox/pull/9))
- Windows Agents can mount host directories. ([#20911](https://github.com/Altinn/altinn-studio/pull/20911), [martinothamar/microsandbox#10](https://github.com/martinothamar/microsandbox/pull/10))
- On Windows, files in shared directories remain accessible after their parent directory is renamed. ([#20911](https://github.com/Altinn/altinn-studio/pull/20911), [martinothamar/microsandbox#14](https://github.com/martinothamar/microsandbox/pull/14))
- On macOS and Linux, stopping an Agent no longer risks corrupting Sandbox state. ([#20911](https://github.com/Altinn/altinn-studio/pull/20911), [martinothamar/microsandbox#12](https://github.com/martinothamar/microsandbox/pull/12))
- In Altinn, self-development and worktree Agents, Rust commands in an Altinn Studio checkout no longer fail with `Permission denied` or need a manual `rustup` update: they use the toolchain the checkout pins, installing it on first use when the image is older. ([#20909](https://github.com/Altinn/altinn-studio/pull/20909))
- Old Agent images no longer fill the disk: `agentd` removes an image 3 days after its last Agent is deleted, so recreating an Agent does not download it again. Images from earlier releases are removed once every Agent has started. ([#20865](https://github.com/Altinn/altinn-studio/pull/20865))
- An Agent whose first start failed no longer fails with `image manifest digest … is not present in this Microsandbox cache` after its image tag, such as `:latest`, moves to a newer version. ([#20865](https://github.com/Altinn/altinn-studio/pull/20865))
- Interrupted commands that wait for an Agent, such as an editor retrying its SSH connection to an Agent that cannot start, no longer make `agentd` stop answering every other command. ([#20884](https://github.com/Altinn/altinn-studio/pull/20884))
- An Agent whose Sandbox stops responding, for example after the host wakes from sleep, shows `SandboxUnresponsive` in `agentctl get agents` and `Unresponsive` in `agentctl tui`. `agentctl exec`, `attach`, `ssh`, `prompt`, `turns` and Session creation then fail instead of hanging, and `agentctl delete` completes. ([#20868](https://github.com/Altinn/altinn-studio/pull/20868))

## [0.1.0-preview.8] - 2026-09-30

### Changed

- Agents run on a newer sandbox runtime, which `agentd` installs by itself; running Agents move to it when they restart. Once the new `agentd` has started, earlier releases cannot read Agent state, so you cannot downgrade. ([#20831](https://github.com/Altinn/altinn-studio/pull/20831))
- On macOS, Agents resolve names through the host's system resolver, so VPN split DNS and `/etc/resolver` domains work inside an Agent as they do on the host. ([#20792](https://github.com/Altinn/altinn-studio/pull/20792))
- `.local` names, reverse lookups of private network addresses and names with non-ASCII characters are no longer resolved through the host, so an Agent cannot discover devices on the host's local network. Names in the Agent's own `/etc/hosts` still resolve. ([#20792](https://github.com/Altinn/altinn-studio/pull/20792))

### Fixed

- Agents resolve names again, without a restart, after the host changes networks or comes back online, for example when a laptop moves between Wi-Fi networks or an Agent was started while the host was offline. ([#20792](https://github.com/Altinn/altinn-studio/pull/20792))
- `agentd` gives back the memory it used to prepare an Agent image once the image is ready. ([#20799](https://github.com/Altinn/altinn-studio/pull/20799))
- Pressing Ctrl-Z in an attached Session no longer freezes it. ([#20830](https://github.com/Altinn/altinn-studio/pull/20830))
- Agents with a direct root filesystem, such as the full Altinn Agent, start again after their VM stops, instead of failing with `VMDK missing` until the Agent is deleted. ([#20831](https://github.com/Altinn/altinn-studio/pull/20831))

### Security

- Changing a running Agent's resources after an `agentd` restart no longer gives the Agent network access that bypasses network authorization and secret mediation. ([#20826](https://github.com/Altinn/altinn-studio/pull/20826))
- An Agent can no longer tunnel non-HTTP traffic through an HTTPS connection to an allowed host to bypass network authorization and secret mediation. WebSocket connections keep working. ([#20831](https://github.com/Altinn/altinn-studio/pull/20831))

## [0.1.0-preview.7] - 2026-09-28

### Added

- `agentctl describe agent` shows the provisioning in progress, or the one that failed with its failing step's output,
  whether a failure is being retried, and how long each condition has held its state.
- Agent status in `agentctl get -o yaml` and `-o json` includes condition transition times, the failure class and
  provisioning progress.
- Provisioning shows Agent setup and SSH access as phases of their own.
- The `agentctl` terminal UI is a live triage view: every Agent and Session with its state and how long it has been in
  it. Sessions that need input are marked and counted, `tab` jumps to the next one, `/` filters, and `?` lists every
  key.
- In the terminal UI, a side panel shows the selected Session's recent turns or the selected Agent's status. `p` follows
  an Agent's provisioning, which also opens for an Agent created with `c`, or prompts a Session without attaching.
- Altinn Agents include `typos` and `hunspell`, so the repository spell check (`yarn spell:quick`, `yarn spell:check`
  and the pre-commit hook) runs inside an Agent.
- The full Altinn Agent includes `cargo-machete`, so `make deps-check` and `make check` in the Rust workspaces run
  inside an Agent.
- `agentctl delete session/<name>` and `d` in the terminal UI delete one Session: its harness is stopped and its name
  becomes free. The harness's own conversation files stay in the Sandbox.
- `agentctl archive session/<name>` and `a` in the terminal UI archive a Session: its harness stops once any turn in
  progress ends, and it is hidden until `agentctl unarchive`. `get sessions --archived` and `A` show archived Sessions.
- A new `desktop` Altinn Agent has a graphical screen it can see and use, driven by a `desktop` helper and a
  `computer-use` skill: screenshot, zoom, point, scroll and type, including Norwegian text, and read what is showing
  as an accessibility tree, including the browser's own controls and dialogs. A terminal opens with `Ctrl+Alt+T`
  or the panel's launcher and has the Session's environment.
- Agents can declare `access: [{type: vnc}]`. Watch or take over the desktop with `agentctl vnc --web`, in a browser
  with nothing installed, or `agentctl vnc` for a VNC client of your own.

### Changed

- Commands that wait for an Agent, such as `apply --wait`, pick up provisioning already in progress and no longer drop
  output when they fall behind.
- Image pulls and imports show downloading, materializing and assembling as separate steps.
- The terminal UI updates as Agents and Sessions change instead of every two seconds, and keeps the last state on screen
  while `agentd` is unreachable.
- Terminal UI forms share one layout with aligned fields, and `NO_COLOR` turns off color while every state keeps its
  glyph.
- New full Altinn Agents finish setup faster. Chromium's trust in the certificate bundle is imported faster and in the
  background, so Sessions no longer wait for it.

### Fixed

- The terminal UI keeps the selection on the same Agent or Session when rows move or an Agent is folded.
- A Claude Code Session left Idle for more than 30 days resumes its conversation instead of starting a new one. Claude
  Code no longer deletes transcripts it considers old.
- The terminal UI's new-Session form rejects a name the Agent already uses instead of attaching to that Session.

## [0.1.0-preview.6] - 2026-09-23

### Changed

- Altinn, self-development, minimal and worktree Agents install Claude Code 2.1.280.
- Altinn, self-development and worktree Agents install Codex CLI 0.156.0, with workspace routing and activity hooks updated for its startup flow.

## [0.1.0-preview.5] - 2026-09-22

### Added

- Codex and Claude Code Sessions show their model, working directory, Git branch, context usage, usage limits, harness
  version and Fast mode in a persistent status line.
- Altinn Agents can authenticate ordinary HTTPS Git commands to Azure DevOps with an optional host-mediated personal
  access token, including cloning the `altinn-studio-infra` repository without exposing the token in the Sandbox.
- Altinn and self-development Agents include Neovim with line numbers, cursor highlighting, a filetype statusline, the
  `habamax` theme and built-in syntax highlighting for C#, JavaScript, TypeScript, JSON and XML.
- Agent images include the `gh stack` extension for creating and managing stacked pull requests.
- The `agentctl` terminal UI supports mouse selection, scrolling, clickable controls and deliberate double-click
  actions while retaining all keyboard controls.
- Agent Skill entries may declare an installed `name` separately from their source directory.
- A harness installation may be declared `optional`, so an Agent is created without it when its host login is
  absent. Altinn Agents declare Codex this way, and signing in on the host installs it on the next convergence.
- Altinn Agents install the repository's text-review and Norwegian copy-editing Skills, so a Session has them as
  well as a local checkout.
- Agent manifests may mark a mediated secret as optional, so an absent or empty value omits that binding instead of
  blocking Agent provisioning.
- Altinn Agent images include `studioctl`, the Altinn Studio app-development skill and `/home/agent/code/apps` for
  app checkouts. They log `studioctl` in to each configured production, staging or development Studio environment
  with a host-mediated API key. Full images also prepare LocalTest hostnames for browser testing.

### Changed

- Pull request evidence guidance is shorter, with readable pacing and no fixed clip
  duration. The GIF conversion helper now accepts files up to 10 MiB instead of 8 MiB.
- Altinn, self-development, minimal and worktree Agents install the latest stable Claude Code and Codex CLI
  harnesses, and Codex command failures remain visible in `agentctl turns` with the new transcript format.

### Fixed

- Agent Sessions set `XDG_RUNTIME_DIR`, so `skopeo`, `buildah` and other tools that expect a user runtime
  directory run instead of failing with a permission error on a path they cannot read.
- `podman run --init` works in full Agents; the `catatonit` binary the flag needs was missing from the image.
- Chromium in a full Altinn Agent trusts the same host-mediated certificate authorities as command-line tools, so
  browser tests can load HTTPS dependencies without disabling certificate verification.

## [0.1.0-preview.4] - 2026-09-18

### Added

- The release installers accept `AGENT_INSTALL_MODE=standalone` to verify and copy only `agentctl` and `agentd` into
  `AGENT_INSTALL_DIR`. This supports immutable images and CI jobs without creating self-update state, migrating Agent
  data, starting the daemon, or changing `PATH`.
- Agent manifests support chained `AgentVariant` files named `agent.<variant>.yaml`. Select them with `--variant` or the TUI, which also supports ignored local variants and an environment file.
- SSH access to Agents. Declare `spec.access: [{type: ssh}]`, then `agentctl ssh <agent> [-- command]` opens a shell or runs a command in the Sandbox as `agent`. `agentctl ssh-config install` lets plain `ssh`, `sftp` and editors reach the Agent as `agentctl-<name>`, and `agentctl ssh-info <agent> -o json` prints the connection details. The Altinn Agent images and the examples declare it; an Agent created from an older image must be deleted and re-applied.
- Windows contributors can run `.\make-user-install.ps1` to build, package and install a local Agent without Make.

### Changed

- `agentctl apply` defaults to `./agent.yaml`. The self-development and Altinn Agents provide nested and worktree variants; Altinn also provides nested-build variants.

### Fixed

- SSH shells, remote commands and editor terminals now inherit the same Agent tool, configured environment and
  mediated certificate settings as Sessions and `agentctl exec`.
- Concurrent network requests from an Agent no longer intermittently fail with DNS, HTTP or TLS errors, especially on Windows hosts.
- Deleting an Agent no longer logs a panic when its Sandbox has an active network-control connection.
- The self-development Agent examples build with their SSH configuration, so the checkout, worktree and nested variants can be applied.
- On Windows, detaching from a Session with `Ctrl-b d` returns control to the terminal UI without dropping the next key press.
- Attached Sessions support mouse-wheel scrolling through up to 50,000 lines of terminal history for new panes. Codex and Claude Code keep their conversations in that history; Claude Code no longer uses its fullscreen renderer, which could corrupt the display when scrolling in tmux. Reattaching enables mouse support for existing Sessions, but cannot recover discarded output.
- Agent setup now writes Sandbox files only when their contents changed, and replaces them atomically. Codex no longer reports missing skill frontmatter after each reconciliation pass.

### Security

- Applying an Agent rejects bind mounts containing `.env` files, case-insensitively and regardless of ignore rules.

## [0.1.0-preview.3] - 2026-09-17

### Added

- `agentctl create` and `agentctl attach` accept `--model` and `--effort`, and the terminal UI's new-session form has the same fields, to choose the model and effort level a Session's harness launches with. Values are the harness's own, for example `fable` and `high` for Claude Code. `spec.harnesses[].defaults` declares per-installation defaults. The choice is fixed for the Session, applied on every relaunch and resume, and shown by `agentctl get sessions`.

### Changed

- Claude Code Sessions launch on the `fable` alias only when the manifest declares it; the `agents/` manifests and the examples do, and your own manifests need `defaults: { model: fable }` on the Claude Code installation to keep it for new Sessions. Sessions created earlier keep launching on `fable`.
- The Sandbox runtime (microsandbox) was updated. Linux hosts with older system libraries, such as Ubuntu 22.04, can now install it, and a Sandbox that fails to start reports the runtime's own error instead of a bare timeout.
- Agent instructions now tell Claude Code and Codex not to add `Co-Authored-By` or similar AI-attribution trailers to commits and pull requests.
- The Altinn Agent images run on Norwegian local time (Europe/Oslo) instead of UTC, so `date`, file timestamps and log output inside an Agent match the clock where the work is reviewed. An existing Agent keeps the image it was created with; delete and re-apply it to pick this up.

### Fixed

- On Windows, starting a Sandbox with a large root filesystem could take an hour while its disk was copied. The copy now takes seconds.
- Linkerd could not start inside a kind cluster running in a Sandbox because the Sandbox kernel lacked the iptables owner match its proxy-init needs. The match is now built in.
- Building the Agent images, or the minimal and worktree examples, failed with a certificate error where the network inspects TLS, such as inside another Agent. The npm, Yarn, Corepack and Playwright downloads now trust the Agent's certificate bundle while the image is built.
- Test suites and dev servers inside an Agent could fail to start with `user limit (128) on inotify instances reached` before running anything, because the guest kept the kernel's desktop-sized file-watcher limits. The Agent images now raise them to the values the self-hosted CI runners already use.
- Logging a nested Agent into Claude failed with an empty credential. Claude Code hides `CLAUDE_CODE_OAUTH_TOKEN` from the commands it runs, so the documented `agentctl claude login --from-stdin` step had nothing to read. An Agent now also carries its Claude credential as `AGENT_CLAUDE_ACCESS_TOKEN`, matching `AGENT_CODEX_ACCESS_TOKEN`, and the self-development instructions use it.

## [0.1.0-preview.2] - 2026-09-15

### Added

- `agentctl self update` installs a newer release, and `--check` only reports whether one exists. It migrates your Agent state behind a backup, relaunches resumable Sessions, and refuses to run while work is in flight. ([#20397](https://github.com/Altinn/altinn-studio/pull/20397))
- `agentctl apply --wait` stays attached and streams provisioning progress — image pull, build, Sandbox creation, harness setup — until the Agent is Ready. `wait`, `attach` and `exec` show the same progress. ([#20341](https://github.com/Altinn/altinn-studio/pull/20341))
- `agentctl port-forward <agent> [ADDRESS:]LOCAL:GUEST...` forwards local TCP ports into a running Sandbox, and `:GUEST` picks a free local port. In the terminal UI, `f` adds a forward and `F` lists them. ([#20189](https://github.com/Altinn/altinn-studio/pull/20189), [#20245](https://github.com/Altinn/altinn-studio/pull/20245))
- `agentctl create`, `prompt` (with `--wait`) and `turns` drive a Session from the command line without attaching. Prompt text comes from `--prompt`, `--file` or piped standard input. ([#20374](https://github.com/Altinn/altinn-studio/pull/20374))
- Press `c` in the terminal UI to create an Agent, choosing from the manifests found by walking down from the repository root. ([#20242](https://github.com/Altinn/altinn-studio/pull/20242), [#20358](https://github.com/Altinn/altinn-studio/pull/20358))
- `spec.environment` copies declared non-secret values from the manifest's `.env` into the Sandbox. `GIT_USER_NAME` and `GIT_USER_EMAIL` also set the Sandbox user's global Git identity. ([#20416](https://github.com/Altinn/altinn-studio/pull/20416))
- `spec.skills` installs skill directories into the Sandbox and `spec.instructions` accepts several sources. Every image gains asciinema and agg, the full image ffmpeg and a Playwright CLI, and the manifests a `pr-evidence` skill. ([#20348](https://github.com/Altinn/altinn-studio/pull/20348))
- `agentctl apply --env-file` names the file supplying the manifest's declared environment and secret values, instead of the `.env` beside the manifest. Keep files holding secrets outside bind-mounted directories. ([#20339](https://github.com/Altinn/altinn-studio/pull/20339))
- `agentctl get` and `agentctl describe` accept `-o json`, so tooling can read Agent and Session state instead of parsing the table. ([#20374](https://github.com/Altinn/altinn-studio/pull/20374))
- `agentctl claude login --from-stdin` and `agentctl codex login --from-stdin` take a credential from standard input, so an Agent can log in a nested Agent without ever holding a real one. ([#20339](https://github.com/Altinn/altinn-studio/pull/20339))

### Changed

- **Upgrading from preview 1.** Stop the running `agentd` first — preview 1 cannot stop itself — then re-run `install.sh` or `install.ps1` once, which migrates your Agents and stored logins. Use `agentctl self update` from then on. ([#20397](https://github.com/Altinn/altinn-studio/pull/20397))
- **A host release does not change your Sandbox image.** The `agents/` manifests reference `:latest`, and an existing Agent keeps the image it was created with. Delete and re-apply the Agent to pick up a newer one.
- `agentctl` explains failures instead of reporting them bare: the manifest setting at fault, the failing image build step, or the Agent's own reason when a wait times out. ([#20341](https://github.com/Altinn/altinn-studio/pull/20341))
- The hostname inside a Sandbox is the Agent name, so prompts and logs read `agent@<name>`. Existing Sandboxes keep their old hostname until they are recreated. ([#20363](https://github.com/Altinn/altinn-studio/pull/20363))
- `spec.harnesses[].version` is optional: the image owns the harness version, so an image bump needs no manifest change. The `agents/` manifests no longer pin one. ([#20250](https://github.com/Altinn/altinn-studio/pull/20250))
- A Session goes idle after 30 minutes rather than 5, and the timer restarts on activity. Codex no longer checks for its own updates at startup. ([#20374](https://github.com/Altinn/altinn-studio/pull/20374))
- Git in the published images authenticates through the `gh` CLI, so `git push` works in a fresh Sandbox without `gh auth setup-git`. The token is also allowed against `gist.github.com`. ([#20151](https://github.com/Altinn/altinn-studio/pull/20151))

### Fixed

- Sessions launch Claude Code on the `fable` alias. Fable never appeared in the `/model` picker, so it could not be selected at all, and a pinned generation would not follow new releases. ([#20147](https://github.com/Altinn/altinn-studio/pull/20147), [#20314](https://github.com/Altinn/altinn-studio/pull/20314))
- Agents start on macOS hosts with long home directory paths, where the Microsandbox control socket could exceed the 104-byte limit on Unix socket paths. ([#20411](https://github.com/Altinn/altinn-studio/pull/20411))
- Configuring Podman waits for the guest to finish booting, instead of failing once with `System has not been booted with systemd as init system`. ([#20346](https://github.com/Altinn/altinn-studio/pull/20346))
- Installing on Windows no longer fails while `install.ps1` verifies the downloaded release archive. ([#20143](https://github.com/Altinn/altinn-studio/pull/20143))

## [0.1.0-preview.1] - 2026-08-26

### Added

- First public preview: `agentctl` and `agentd`, declarative Agents with durable tmux-backed Sessions in isolated Sandboxes, mediated harness and GitHub authentication, and the published Agent images.
