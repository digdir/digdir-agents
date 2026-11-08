# agentctl docs

| Doc | Kind | What it is for |
| --- | --- | --- |
| [first-agent.md](first-agent.md) | Tutorial | Give a repository an agent, from an empty folder to the repository open in your editor inside the agent |
| [open-in-an-editor.md](open-in-an-editor.md) | How-to | Open an agent's project in VS Code, Zed or another editor that works over SSH |
| [open-in-jetbrains.md](open-in-jetbrains.md) | How-to | Open an agent's project in a JetBrains IDE, such as Rider, through JetBrains Gateway |
| [label-agents.md](label-agents.md) | How-to | Label agents for the client you open them in or the tool that created them, and list agents by label |
| [variant-lists.md](variant-lists.md) | How-to | Add, change or remove one entry of an inherited list, such as `access` or `harnesses`, in a variant |
| [web-views.md](web-views.md) | How-to | Declare the web UIs an agent serves, so clients can list them |
| [find-agents-from-a-client.md](find-agents-from-a-client.md) | How-to | Write a client that finds the agents meant for it, and what to open in each |
| [harness-upgrades.md](harness-upgrades.md) | How-to | Updating a harness installation or adapter, and the test plan for it |
| [moving-off-the-environment-file.md](moving-off-the-environment-file.md) | How-to | Moving the values an agent reads from its `.env` to accounts, stored secrets and the manifest |
| [agent-resource.md](agent-resource.md) | Reference | The agent fields clients read, `ssh-info` and `get definitions` output, and the syntax of labels, selectors and variant lists |
| [image-contract.md](image-contract.md) | Reference | What an agent image must provide that `agent-image-check` cannot check |
| [variants.md](variants.md) | Reference | How an agent variant changes the manifest it extends, and where that differs from Kustomize |
