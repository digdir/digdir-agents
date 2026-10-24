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

Keep changes targeted. When the existing design gets in the way, such as code that is hard to change, duplication or an
abstraction that no longer fits, say so as soon as you notice it rather than working around it. Suggest what you would
do about it, and whether to do it before, as part of or after the change. Agents raise it with the person prompting
them. This way the architecture and design evolve with what the changes demand, and the code stays maintainable over
time.

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
2. Once it is merged, run the release workflow manually on `main` with the version, as a dry run.
3. Push the tag `agentctl/vX.Y.Z`. The release workflow publishes that changelog section as the release notes. A tag
   can never be moved or reused.

## Maintenance

- [sandbox/docs/microsandbox-upgrade.md](sandbox/docs/microsandbox-upgrade.md): updating the Microsandbox pin.
- [agentctl/docs/harness-upgrades.md](agentctl/docs/harness-upgrades.md): updating a harness installation or adapter.
