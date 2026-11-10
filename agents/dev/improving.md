## Improving this agent

Your own definition is `agents/dev` in digdir-agents, and its image is published by `.github/workflows/dev-images.yml`.
The `agent-definition` skill applies to it, with one exception: its image builds on the base images' `main` tags
without a digest, so it follows the platform it develops, as the comment at the top of its `Dockerfile` explains.
