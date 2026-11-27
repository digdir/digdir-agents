# Add an agent to a repository

How to give a repository an agent definition, so that anyone on the team gets an agent that builds, tests and runs
the code with one `agentctl apply`. The agent builder writes the definition with you. The example is
[Altinn/altinn-notifications](https://github.com/Altinn/altinn-notifications), a .NET backend with PostgreSQL and
Azure Service Bus, which had no agent definition.

The steps were run on Windows 11 with a build of `main` at `2d646ba` plus
[#478](https://github.com/digdir/digdir-agents/pull/478). The run was driven by a coding agent on the host, which
cannot type into `agentctl new`, so it started the builder with the `agentctl apply --from agent-builder` and
`agentctl create` that `new` runs, and answered in the session's tmux pane. The conversation is the one you get.

## Before you start

- `agentctl` and `agentctl claude login`. The builder runs on your Claude Code login.
- A checkout of the repository, on a branch you can commit the definition to. A
  [worktree](https://git-scm.com/docs/git-worktree) keeps it apart from other work:

  ```sh
  git -C ~/repos/altinn-notifications worktree add ../altinn-notifications-agent -b feature/agent-definition origin/main
  ```

The host needs no Docker. The builder builds and tries the new agent's image inside its own sandbox.

## 1. Start the builder

```sh
agentctl new ~/repos/altinn-notifications-agent
```

`agentd` starts the builder, an agent of its own from the definition `bundled/agent-builder`, with the checkout
mounted at `/home/agent/code`, and attaches you to its `new-agent` session:

```text
agent/agent-builder applied
  ✓ Download image layers (1m 06s)
  ✓ Materialize image layers (1m 06s)
  ✓ Pull OCI image (1m 07s)
✓ Resolve sandbox image (1m 07s)
✓ Create sandbox (168ms)
  ✓ Create Microsandbox VM (4.5s)
✓ Start sandbox (4.6s)
  ✓ Prepare workspace and Podman (1.5s)
✓ Set up agent (2.2s)
✓ Configure SSH access (1.0s)
agent/agent-builder ready
```

When the checkout holds a `.env` file, as `tools/asb-emulator/.env` in altinn-notifications, `agentctl` asks before
it lets the builder read it. Say yes only when the file holds no real key.

The session is Claude Code in tmux. Leave it with `Ctrl-b d`. The builder keeps running, and `agentctl new` in the
same checkout goes back to the same conversation.

## 2. Answer its questions

The builder asks one question at a time and offers what it found in the repository as the default, so most answers
are Enter.

**The role.** It had already read enough of the repository to offer the roles that fit:

```text
 ☐ Role

│ What role should the new agent have on altinn-notifications (a .NET 10
│ backend: API, email and SMS services, with PostgreSQL and Azure Service Bus)?

❯ 1. Backend developer (Recommended)
     Builds the solution and runs the unit and integration tests, including
     those that need PostgreSQL or the Service Bus emulator in containers.
  2. Tester
     Writes and runs tests, including the k6 regression and use-case tests in
     .github.
  3. Reviewer
     Reviews pull requests: builds them, runs the tests and comments on them.
  4. Documentation writer
     Keeps the docs/architecture pages, the READMEs and the Swagger docs up to
     date.
  5. Type something.
```

**What done looks like.** This answer decides what the agent must be able to run, and so how large its image and
its sandbox become. The test chose the second option, because an agent that runs the service is one you can test
against by hand, as [Work with agents](work-with-agents.md#test-by-hand-against-what-the-agent-runs) describes:

```text
 ☐ Done

│ When the backend developer agent finishes a task on altinn-notifications, what
│ should it have run, and what should it show for it?

❯ 1. All CI tests, results in PR (Recommended)
     Builds every solution and runs the unit, integration (PostgreSQL) and
     ASB-emulator integration tests for the components it touched, as CI does.
     The pull request lists the test results.
  2. Tests plus running service
     The same, and it also starts the changed service locally (e.g. via Aspire
     or dev-setup) and shows requests against it, with a terminal recording.
  3. Unit tests only
     Builds and runs the unit tests only, with no containers. Smaller and
     faster, but integration coverage is left to CI.
```

**The team's or personal.** A team's agent goes into `agents/<name>/` in the repository, with a CI workflow that
publishes its image. A personal one goes into `~/agents/<name>` on your machine.

**Other repositories, secrets and harnesses.** The test took the defaults: no other repository, a GitHub account and
no secrets, Claude Code with Codex as an option.

## 3. Check what it found

The builder then studies the repository on its own: build files, CI workflows, the files that pin tool versions,
Compose files and test setups. That took about four minutes. It shows what it found before it writes anything:

```text
● Here is what the repository needs and what I'd build. Please confirm or
  correct it before I write anything.

  Toolchains
  - .NET SDK 10.0.403, as global.json pins it. Every project targets net10.0.
    Ubuntu's own dotnet-sdk-10.0 package is a 10.0.1xx SDK, which global.json
    would reject, so the image installs Microsoft's 10.0.403 archive and checks
    its SHA-512.
  - Aspire CLI 13.6.4, the version tools/aspire/apphost.cs pins, for running the
    whole stack with aspire run -lp http.
  - Podman with docker and docker compose, from the base image's containers
    feature. Testcontainers and tools/asb-emulator use it.
  - psql, which tools/dev-setup/dbsetup.sh uses.

  Checks, and what each needs
  ...
  Size: 4 CPU, 12Gi memory and 64Gi disk. SQL Server alone needs about 2Gi, and
  the emulator, Postgres and the test hosts run beside it.

  Accounts, secrets and network: a GitHub account (default) and no secrets.

  What I'd write
  - agents/backend-dev/, holding the Dockerfile, .dockerignore,
    common/workspace.list, instructions.md, improving.md, a pr-evidence skill
    (test results and a recording of the running service in the PR), and
    minimal/ with agent.yaml, the nested, nested-build and worktree variants,
    environment.md and .gitignore
  - .github/workflows/backend-dev-agent-image.yml, which builds on native amd64
    and arm64 runners and publishes
    ghcr.io/altinn/altinn-notifications/backend-dev-agent-minimal:latest
```

Read this closely. A wrong guess here costs an image rebuild later, and you know things about the repository that
its files do not say. In the test one thing was wrong: an image that pins the SDK patch falls behind, because the
agent clones the repository at boot, after the image is built, and Renovate bumps `global.json` often. An earlier
hand-written definition had met exactly that. Choose **Type something** to correct it:

```text
❯ 4. Yes, with one change: do not pin the SDK patch in the image. The
     repository is cloned at boot, after the image is built, and Renovate bumps
     global.json often, so an image with 10.0.403 falls behind. Install the
     newest 10.0 SDK with dotnet-install.sh --channel 10.0 instead. Otherwise
     go ahead.
```

## 4. Let it write and try the definition

The builder writes the definition into the checkout as new, untracked files:

```text
?? .github/workflows/backend-dev-agent-image.yml
?? agents/
```

```text
agents/backend-dev/
├── Dockerfile                  the image: the base image plus the SDK, Aspire, psql and Podman
├── common/workspace.list       the repository the agent clones at boot
├── instructions.md             what every harness is told about this repository
├── improving.md                how the agent improves its own definition
├── skills/pr-evidence/         how it shows test results and a recording in a pull request
└── minimal/
    ├── agent.yaml              the manifest, on the image CI publishes
    ├── agent.nested.yaml       sized to run inside another agent
    ├── agent.nested-build.yaml nested, with the image built from the checkout
    ├── agent.worktree.yaml     with your checkout mounted instead of a fresh clone
    └── environment.md          what is special about this computer
```

It then builds the image and starts the agent as a nested agent inside its own sandbox, and runs there each check it
promised in step 3. It reports each one as verified, or not and why. This is the slow part: an image build and the
first run of every test suite.

In the test this took the builder three image builds. The first stopped because .NET aborts without ICU, the
second because root-owned files from the Aspire install failed `agent-image-check`. It found and fixed both itself.
Its report at the end says what it verified and, as important, what it did not:

```text
● The backend developer agent for altinn-notifications is written and was tried in a nested agent. The build, unit and
  PostgreSQL integration tests all pass. The Service Bus (ASB) integration tests run but did not pass reliably, and I
  didn't try running a service, which is part of what you asked "done" to include.

  What was tried

  Image builds, passes agent-image-check             Verified on a clean rebuild. .NET SDK 10.0.404, checksum checked.
  Aspire CLI 13.6.4 runs as the agent user           Verified after the rebuild
  All .slnx load, all 5 solutions build (Release)    Verified
  Unit tests (API 1500, Shared 39, Email 122, SMS 63)                All passed
  PostgreSQL integration tests (API 641, Tools 80, Email 16, SMS 18) All passed, using notifications-db
  ASB integration tests                              Run, not reliable on the trial machine
  Claude Code session reads its instructions and skill               Verified
  Running the services, requests against them, terminal recording    Not tried
  Clone at boot                                      Not tried: it needs the GitHub account, which I don't have.
  Pushing and opening PRs, Codex, the arm64 image, the CI workflow   Not tried

  Before you commit

  - The GHCR package starts private. Make it public, or give the team read access, after the first publish, or
    teammates can't pull the image.
  - SQL Server has no arm64 image. On Apple Silicon the ASB tests and the emulator can't run, and the agent is told to
    say so.
```

The ASB tests failed on timing: the nested agent the builder tries in has 3 CPUs and 6 GiB, half of what the agent
itself gets. Take the "not tried" rows as your list of what to try first when you run the agent at its real size.
[Run a T3 orchestrator in an agent](run-a-t3-orchestrator.md#4-start-a-thread) did that for running the services.

## 5. Review, commit and run

The builder has no GitHub token and no Git identity. It leaves the files for you:

```sh
cd ~/repos/altinn-notifications-agent
git status
git add agents .github/workflows/backend-dev-agent-image.yml
git commit -m "Add the backend developer agent"
```

Open a pull request. Once it merges, CI publishes the image the manifest references, and everyone on the team runs
the agent with:

```sh
agentctl github login default      # once per machine
agentctl apply -f agents/backend-dev/minimal/agent.yaml --wait
```

To run it before that, build the image on your own machine, which needs Docker there:

```sh
agentctl apply -f agents/backend-dev/minimal/agent.nested-build.yaml --wait
```

Then delete the builder, or keep it to go back to. `agentctl new` prints its name when you leave the session:

```sh
agentctl delete agent/NAME
```

## Keep the definition current

- After `agentctl self update`, run `agentctl update` in the checkout. The builder reads the release notes since the
  version the definition was written for and proposes each change.
- The agent improves its own definition too. `improving.md` tells it how: when a task shows that a tool is missing,
  it changes the Dockerfile, tries the result as a nested agent and opens a pull request.

## What went wrong in the test

- **The session's state says `WaitingForInput` while the builder works.** After its first question,
  `agentctl get sessions` and the TUI showed the session as waiting for you for the rest of the conversation, also
  through minutes of work. Look at the session itself to see whether it wants an answer.
- **A session that went idle cannot be prompted.** After a pause of two hours the session was `Idle`, and
  `agentctl prompt` refused it with `its lifecycle is idle`. Only `agentctl attach`, which needs a terminal, resumed
  it.
- **The builder cannot use Git in a linked worktree.** The worktree's `.git` file points at a folder on the host
  that is not mounted, so the builder could not run `git diff` or `git status` on what it wrote.
- **`agentctl exec` did not pass standard input through** in the builder's sandbox, so a script it piped into the
  nested agent arrived empty, and it waited for results that never came until told.
- **`agentctl turns` does not show a question's text.** It prints `[assistant] -> AskUserQuestion`, so a client that
  drives the builder with `agentctl prompt` and `turns` cannot tell what it asked. `agentctl new` refuses to run
  without a terminal for that reason.
