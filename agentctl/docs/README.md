# agentctl docs

| Doc | What it is for |
| --- | --- |
| [image-contract.md](image-contract.md) | What an agent image must provide that `agent-image-check` cannot check |
| [harness-upgrades.md](harness-upgrades.md) | Updating a harness installation or adapter, and the test plan for it |
| [moving-off-the-environment-file.md](moving-off-the-environment-file.md) | Moving the values an agent reads from its `.env` to accounts, stored secrets and the manifest |
| [variants.md](variants.md) | How an agent variant changes the manifest it extends, and where that differs from Kustomize |
| [design/client-discovery.md](design/client-discovery.md) | What the agent resource and the Control API could add so that every client finds the agents meant for it the same way |
| [design/jetbrains-gateway.md](design/jetbrains-gateway.md) | How `agentctl` and the base images could open an agent in JetBrains Gateway without manual steps |
