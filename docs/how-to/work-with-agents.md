# Work with agents

How a developer works once the agents are there: hand work to an orchestrator, look closer from an editor or a
shell when something needs it, and test what the agents built, automatically and by hand. The examples continue
from [Run a T3 orchestrator in an agent](run-a-t3-orchestrator.md), with
[Altinn/altinn-notifications](https://github.com/Altinn/altinn-notifications) joined to `t3-master`.

## Three ways in, one computer

Every way in reaches the same sandbox, so you choose by what you want to see, not by where the work is.

| You want to | Use | Reaches |
| --- | --- | --- |
| Hand over a task and follow several at once | The orchestrator: T3 Code in the browser | `t3-master`, which delegates to its workers |
| Read a diff as it grows, debug, run one test again | An editor over SSH | Any agent, as [Open an agent in an editor](open-an-agent-in-an-editor.md) describes |
| See what a harness is doing, or run a command | `agentctl tui`, `attach`, `turns`, `ssh`, `exec` | Any agent and any session in it |
| Call what the agent runs | `agentctl port-forward`, then Bruno, k6, curl or a browser on the host | A port in the agent |

Nothing in the list is exclusive. A worker T3 Code created is an ordinary agent: you can attach to its session, open
it in an editor and forward a port from it while the orchestrator keeps working in it.

## Hand work to the orchestrator

Start a thread in the repository's project in T3 Code and describe the outcome, not the steps. Say what done looks
like, as you did when the agent was made: which tests must pass, and what the pull request must show.

The thread runs in `t3-master`. For work that needs the repository's toolchain, it creates a worker from the
repository's definition and delegates to it. Work that spans repositories gets one worker per repository, each with
that repository's tools, and the thread carries what one found to the other.

The thread comes back with what the worker found and the decisions that are yours, as the run in
[Run a T3 orchestrator in an agent](run-a-t3-orchestrator.md#4-start-a-thread) shows: asked to start the stack, the
worker found that the repository's Aspire pin had a new security advisory that stops the build, and the thread asked
whether to bump the pin or to hide the warning.

## Look closer

### From the host's shell

`agentctl tui` lists every agent and session on the host, with what each waits for:

```text
 agentctl  1 need you · 1 provisioning
  NAME             STATE         FOR DETAIL
▾ agent-builder    Ready          8m 1 session
  ! new-agent      Needs you      7m Claude Code
▾ altinn-minimal   Ready          8m
▾ t3-master        Provisioning  15s Resolve sandbox image · Build
```

`enter` attaches to a session, `o` opens the agent in an editor or a shell, `e` runs a command, `f` forwards a
port, `p` shows provisioning and `s` describes the agent. The same from a script or another terminal:

```sh
agentctl get agents
agentctl get sessions -A                              # every session on every agent
agentctl describe agent/altinn-minimal                # conditions, and why it is not ready
agentctl turns session/new-agent --agent agent-builder --last 1   # what the harness said and did
agentctl attach session/new-agent --agent agent-builder           # the harness itself; Ctrl-b d detaches
agentctl ssh altinn-minimal                           # a shell beside the sessions
agentctl exec agent/altinn-minimal -- systemctl --failed
```

`turns` reads a conversation without attaching, which is how you check on an agent without disturbing it:

```text
[assistant] The image is building and the nested agent will start after it. That takes several minutes.
[assistant] -> Bash
[assistant] The SDK step aborted with exit status 134 (SIGABRT). That is likely the `dotnet` host failing on a
            missing ICU library. I'll reproduce it in the base image.
[assistant] -> Bash
```

### Workers inside the orchestrator

The workers belong to the `agentd` in `t3-master`, so the host's `agentctl` does not list them. Run the same
commands one hop in:

```sh
agentctl ssh t3-master -- agentctl get agents
agentctl ssh t3-master -- agentctl describe agent/notifications-backend-dev-nested-1
agentctl ssh t3-master -- agentctl turns session/NAME --agent notifications-backend-dev-nested-1 --last 1
```

For a shell in a worker, open one in `t3-master` first:

```sh
agentctl ssh t3-master
agentctl ssh notifications-backend-dev-nested-1      # now in the worker
```

### From an editor

Open the agent the work happens in. For a worker in the orchestrator, open `t3-master` and work in
`~/code/<repository>`, the checkout its threads use:

```sh
code --remote ssh-remote+agentctl-t3-master /home/agent/code/altinn-notifications
```

## Test automatically

The agent tests its own work. That is what its definition is for: the answer to "what does done look like" became
the toolchain in its image, the checks in its instructions and the evidence its pull requests show. For
altinn-notifications the backend developer agent builds every solution, runs the unit tests, the integration tests
against PostgreSQL and the Service Bus integration tests with Testcontainers, and puts the results in the pull
request.

Your part is to ask for it and to read it:

- Say in the task which checks must pass. An agent that is told "run the integration tests for the API" reports
  them. One that is told "fix the bug" may stop at the unit tests.
- Read the evidence in the pull request before the diff. A missing test run is the first thing to send back.
- When a check cannot run in the agent, the fix belongs in the definition, not in the task. Ask the agent to
  improve its definition, as its `improving.md` describes, so the next task has the tool.

CI still runs on the pull request. The agent's run makes the first push the one that passes.

## Test by hand against what the agent runs

Some things you want to try yourself: a request you are unsure of, a load profile, the feel of an API. The agent
runs the service, and you call it from the host with the tools you already use.

### 1. Have the agent run the service

Ask for it in the thread: "start the whole stack with Aspire, leave it running, and report which ports answer". In
the run the thread delegated that to the worker and came back with:

```text
The whole stack is up and still running on the worker, and the API's /health returns 200 Healthy.

Port          What listens                    State
5432          PostgreSQL (notifications-db)   up
5090          Notifications API               /health 200
5091          Email service                   /health 200
5092          SMS service                     /health 200
5672 / 5300   Service Bus emulator            up
15193         Aspire dashboard                listening

Start timeout: the first run built cleanly but took longer than the Aspire CLI's 120-second start limit, so it
exited. The worker reran it with ASPIRE_CLI_START_TIMEOUT=600, and /health answered after about 6 minutes.
```

The ports listen on the worker's own `localhost`.

### 2. Forward the port

For an agent on the host, one command, left running:

```sh
agentctl port-forward notifications-backend-dev 5090:5090
```

A worker in the orchestrator is two hops away, so forward twice: from the worker to `t3-master`, and from
`t3-master` to the host. Each in its own terminal, left running, the inner one first:

```sh
agentctl ssh t3-master -- agentctl port-forward notifications-backend-dev-nested-1 5090:5090
```

```text
Forwarding from 127.0.0.1:5090 -> 5090 (agent "notifications-backend-dev-nested-1")
```

```sh
agentctl port-forward t3-master 127.0.0.1:5090:5090
```

```text
Forwarding from 127.0.0.1:5090 -> 5090 (agent "t3-master")
```

### 3. Call it

```sh
curl -i http://127.0.0.1:5090/health
```

```text
HTTP/1.1 200 OK
Content-Type: text/plain
Server: Kestrel

Healthy
```

That answer came from the API the worker runs from its working tree, two sandboxes in. The Swagger document is on
`/swagger/v1/swagger.json`, and an order request without a token gets `401`, as it should.

**Bruno.** The repository's collection is in `components/api/test/bruno`, and its `v2 local` environment already
points at `http://localhost:5090`. Open the collection from your checkout on the host, choose that environment and
send requests as you would against a service on your own machine. The secrets the environment needs stay where you
keep them on the host. None of them enters the agent. The run sent the requests with `curl`, not from Bruno's
window.

**k6.** Run it on the host against the forwarded port. With k6 in a container, the host is `host.docker.internal`:

```sh
docker run --rm -e BASE_URL=http://host.docker.internal:5090 -v "$PWD:/scripts" grafana/k6 run /scripts/health.js
```

```js
import http from "k6/http";
import { check } from "k6";

export const options = { vus: 5, duration: "10s" };

export default function () {
  const response = http.get(`${__ENV.BASE_URL}/health`);
  check(response, { "status is 200": (r) => r.status === 200 });
}
```

```text
    ✓ status is 200

    http_req_duration..............: avg=5.76ms min=2.76ms med=5.21ms max=58.66ms p(90)=7.49ms p(95)=8.71ms
    http_req_failed................: 0.00%  0 out of 8470
    http_reqs......................: 8470   846.686401/s
```

The repository's own tests in `components/api/test/k6` run the same way, with the command at the top of each test
file and the local base URL. They need a token generator, which the run did not set up.

The requests leave your machine, pass the forwards and reach the service the agent runs. So you test exactly the
change the agent is making, before it is pushed, and what you find goes back as the next message in the thread.

### The other direction

When the agent should run the Bruno or k6 tests itself, they are an automatic check: add them to what done looks
like, and the tools to the definition.

## What went wrong in the run

- **The repository's main branch did not start.** Aspire 13.6.4, which `tools/aspire/apphost.cs` pins, got a
  security advisory, and the repository treats warnings as errors. The worker found it, and bumped the pin on a
  local branch when asked.
- **The first request through a new forward was refused.** The host's forward answered before the inner one
  listened. Start the inner forward first.
- **The nested variant is small for the whole stack.** With 3 CPUs and 6 GiB the first start took six minutes and
  left no free memory. Give a worker that runs the stack the agent's full size.
- **A commit in a worker has no author** until the host has `agentctl github login default`: the worker committed as
  `agent <agent@notifications-backend-dev-nested-1>`.
