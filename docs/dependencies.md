# Dependencies

What we update, how often and how, and why. Moving the Microsandbox pin is in
[microsandbox-upgrade.md](../sandbox/docs/microsandbox-upgrade.md).

We update in small steps, often. A week of updates is quick to review, and few changes are suspect when one breaks. A
year of them is breaking changes stacked on code nobody remembers, and turns a security fix into a migration.

| What | When | How |
| --- | --- | --- |
| Security advisories | Right away | The smallest update that fixes it |
| Compatible crate updates | Weekly | One `cargo update` pull request |
| Breaking crate updates | Within a release or two | One crate per pull request |
| Rust toolchain | Each stable release | Every pin in one commit |
| Microsandbox fork | Each upstream release | A synchronization |
| Base images | As their digests move | Renovate's digest updates |

## Pinning

We pin everything that has a pin, so a change to what we build is a diff someone reviews: crates in `Cargo.lock`, which
builds read with `--locked`, the toolchain, images by the digest of their multi-arch index, GitHub Actions by commit SHA
and Git dependencies by revision. Packages an image build installs from its distribution have no pin, and stay current
only as long as we [rebuild](#base-images).

## Security advisories

`cargo deny` in CI fails a pull request when a crate we compile has an advisory. Fix it right away with the smallest
update that removes it. When there is no fix, or the affected code is unreachable from ours, ignore it in
[deny.toml](../deny.toml) with the reason and what would remove it.

## The weekly batch

Once a week, `chore(deps): update Cargo dependencies` takes everything a plain `cargo update` resolves within our
version requirements, including new major versions deeper in the tree. CI reviews it, since there are too many updates
to read each changelog and one at a time they rarely break anything. The description names what stands out, such as
removed crates and 0.x minor bumps of crates we use directly.

Microsandbox's crates build from our lockfile, so the batch also changes the sandbox runtime and the mediated network,
which CI does not fully exercise. Before it merges, install its build with `make user-install`, start an agent and run a
command in a session. Hold a crate that breaks back with `cargo update -p <crate> --precise <old>`, say why, and give it
its own pull request.

One person owns the batch each week, even when an agent does the work. Without an owner it waits, and the next one
grows.

## Breaking updates

Moving a requirement in our `Cargo.toml` to a new major version, or a new minor version of a 0.x crate, gets its own
pull request. Read the changelog and say what changed for us. One crate at a time keeps a failure attributable. Stay at
most a release or two behind, before breaking changes pile up and the old release stops getting fixes.

For a crate we share with Microsandbox, use the version Microsandbox uses. Two versions of one crate build twice, and
neither's types fit the other's APIs.

## Toolchain and forks

Microsandbox's crates build with our toolchain, and upstream tests them on the current stable. So we take each stable
release, and never fall behind the fork's pin. Move every pin in one commit, since a partial move leaves CI, the images
and checkouts on different compilers, and fix new Clippy findings in the same pull request. To find the pins:

```sh
git grep -n '<old-version>' -- ':!Cargo.lock'
```

The Microsandbox fork follows upstream releases, and the libkrunfw fork follows the revision each one pins, as their
maintenance guides, [Microsandbox's](https://github.com/digdir/microsandbox/blob/main-digdir/MAINTAINING-digdir.md) and
[libkrunfw's](https://github.com/digdir/libkrunfw/blob/main-digdir/MAINTAINING-digdir.md), describe. On each
synchronization, check the toolchain, the crates we share and the kernel configuration again, then move our pin.

## Base images

A digest pin freezes a base image's packages, security fixes included, and a published image keeps those it was built
with. Renovate's digest updates move the pins, and merging one rebuilds the images on that base with the distribution's
current packages.

## New dependencies

A new dependency is code we build, ship and keep updating, and a way in for a compromised release. Prefer the standard
library, a crate already in the lockfile or a few lines of our own. Otherwise prefer a maintained, widely used crate
with few dependencies, enable only the features we use, and say why in the pull request.

## Renovate

[Renovate](../renovate.json) opens update pull requests weekly, and security updates as they come:

- A compatible Cargo update: the weekly batch includes it, and its owner closes the Renovate pull request with a link to
  the batch.
- A breaking Cargo update: take it over, read the changelog and fix the code on its branch.
- An image or a GitHub Action: merge it when CI passes.
- A security update: merge it right away.
