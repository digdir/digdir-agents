---
name: aspnet-backend
description: Build and test ASP.NET Core backends, and run what they depend on in containers with Docker Compose. Use when building or testing a .NET solution, or starting the services its tests or a local run need.
---

# ASP.NET Core backend work

A repository's own scripts, `Makefile`, CI workflow or `README` know its build better than this skill. Read them
first, and use this skill for what they leave out.

## Build and test

```sh
dotnet tool restore
dotnet restore
dotnet build --no-restore
dotnet test --no-build
```

- Run `dotnet tool restore` only when the repository has a tool manifest, `.config/dotnet-tools.json`.
- Narrow a test run while you iterate with `--filter`, such as `--filter "FullyQualifiedName~OrderService"`, then run
  the whole affected test project before you report the change done.
- A `global.json` pins the SDK. When `dotnet --version` reports an error about it, the pinned major is not installed:
  say so rather than editing `global.json`.

## Formatting

Use `dotnet format`, which follows the repository's `.editorconfig`, unless the repository uses a formatter of its
own. Do not reformat files you did not change.

## Containers

`docker` and `docker compose` run Podman here. Start only the services you need and wait for them to be healthy:

```sh
docker compose up --detach --wait <service>
docker compose ps
docker compose down --volumes
```

- Name images by their full registry path, such as `docker.io/library/nginx:1.29-alpine`. Podman does not guess a
  registry for a short name.
- Tests that start containers themselves find Podman through `/run/docker.sock` without configuration. A test that
  hangs at start is usually waiting for an image to pull: pull it first with `docker pull` to see why.
