# Standard agents

The agents `agentctl setup` offers to start from. Each directory holds one agent definition, and
[catalog.yaml](catalog.yaml) describes them for the setup wizard and the KI-hub directory site.

Each agent's manifest references a published image, so the host needs no Docker to run it. CI builds the images on
the base image from the same commit and publishes them from `main`, as
[agentctl-standard-images.yml](../../.github/workflows/agentctl-standard-images.yml) describes. When an image cannot
be pulled, such as before it is published, setup offers to build it on the host from the agent's Dockerfile instead.
That build gets the files beside the Dockerfile, not those in folders below it, so keep what the image copies there.

To add one, give it a directory with its manifest and Dockerfile, an entry in `catalog.yaml`, a `standard-<id>` target
in the `standard` group of [docker-bake.hcl](../../docker-bake.hcl), and its ID in the workflow's `AGENTS`. Keep its
entry `hidden` until its image is published.
