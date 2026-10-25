# Contributing

## Development

Run `make help` at the repository root for the development targets and what each one needs. `make check` runs all
local checks.

`make user-install` builds, packages and installs `agentctl` and `agentd` for the current user. On Windows without
Make, run `.\agentctl\make-user-install.ps1`.

## Code

Follow the [architecture principles](docs/architecture.md#principles) and the
[coding standards](docs/coding-standards.md).

### Design feedback

We want the design to improve as the code changes, not to be worked around. When the code makes your change bigger or
trickier than it needs to be, stop and think about whether a refactor or
[semantic compression](docs/coding-standards.md#design) would remove that accidental complexity. Whether and when is
up to you. Small ones often fit in the same pull request. Larger ones are easier to review on their own, before your
change if they make it simpler, or after if they can wait. Agents talk through the larger ones with the person
prompting them.

## Pull requests

- Titles follow Conventional Commits with the area as scope, such as `feat(agentctl): ...` or `fix(agentd): ...`. Mark
  breaking changes with `!`.
- A change users of `agentctl` or its Agents will notice gets an entry under `## [Unreleased]` in
  [agentctl/CHANGELOG.md](agentctl/CHANGELOG.md), written as the
  [changelog skill](agents/self-dev/skills/changelog/SKILL.md) describes.
- Write the description for the reviewer, following the [pull request template](.github/pull_request_template.md).
  Agents record demonstrations as the [pr-evidence skill](agents/self-dev/skills/pr-evidence/SKILL.md) describes.
- [CODEOWNERS](.github/CODEOWNERS) decides which paths need a human approval.
- Comment `/agentctl-experimental` on a pull request to build an unlisted version of it that others can install.
- CI skips draft pull requests. Add the `ci` label to a draft to run CI on it from its next push.

## Documentation

Follow the [documentation guidelines](docs/documentation.md).

## Changelog and releases

[agentctl/CHANGELOG.md](agentctl/CHANGELOG.md) is the release notes for the Agent platform, released as `agentctl`
and `agentd` under the `agentctl/v*` tag. `make changelog-validate` checks it.

To release:

1. Open a pull request that renames `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD` and adds an empty
   `## [Unreleased]` above it.
2. Once it is merged, run the [release workflow](.github/workflows/agentctl-release.yml) manually on `main` with the
   version `vX.Y.Z` and `publish` checked. It publishes that changelog section as the release notes, and creates the
   tag `agentctl/vX.Y.Z` only once everything is built, since a tag can never be moved or reused. Leave `publish`
   unchecked for a dry run.

## Maintenance

- [sandbox/docs/microsandbox-upgrade.md](sandbox/docs/microsandbox-upgrade.md): updating the Microsandbox pin.
- [agentctl/docs/harness-upgrades.md](agentctl/docs/harness-upgrades.md): updating a harness installation or adapter.
