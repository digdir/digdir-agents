# agentctl docs

| Doc | What it is for |
| --- | --- |
| [agentd-configuration.md](agentd-configuration.md) | The settings `agentd` reads from `agentd.toml`, such as when it warns that the host's disk is low |
| [image-contract.md](image-contract.md) | What an agent image must provide that `agent-image-check` cannot check |
| [harness-upgrades.md](harness-upgrades.md) | Updating a harness installation or adapter, and the test plan for it |
| [moving-off-the-environment-file.md](moving-off-the-environment-file.md) | Moving the values an agent reads from its `.env` to accounts, stored secrets and the manifest |
| [terminals-through-agentd.md](terminals-through-agentd.md) | Proposal: `agentd` runs every terminal in an agent and relays it over the Control API, so clients stop opening the sandbox |
| [variants.md](variants.md) | How an agent variant changes the manifest it extends, and where that differs from Kustomize |
