# Standard agents

Agents for a kind of work, for anyone to start from. Each directory holds one agent definition, and
[catalog.yaml](catalog.yaml) describes them for the KI-hub directory site.

Each agent's manifest references a published image, so the host needs no Docker to run it. CI publishes the images
from `main`, as [standard-images.yml](../../.github/workflows/standard-images.yml) describes.

To add one, give it a directory with its manifest and Dockerfile, an entry in `catalog.yaml`, a `standard-<id>` target
in the `standard` group of [docker-bake.hcl](docker-bake.hcl), and an entry in the workflow's `images`. Keep its
entry `hidden` until its image is published.
