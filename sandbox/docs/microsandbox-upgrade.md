# Microsandbox upgrade

`sandbox-microsandbox` builds on the Digdir fork of Microsandbox,
[digdir/microsandbox](https://github.com/digdir/microsandbox), and on the host runtime published with each of its
releases. The fork's
[MAINTAINING-digdir.md](https://github.com/digdir/microsandbox/blob/main-digdir/MAINTAINING-digdir.md#updating-consumers)
says which revisions a consumer may pin and when. This document says where the pin lives in this repository and how
to move it.

The pin is the `microsandbox*` Git revision in the root `Cargo.toml` and `Cargo.lock`, and the runtime bundle digests
in [`client.rs`](../microsandbox/src/client.rs). They always come from one Digdir release, since
`sandbox-microsandbox` only launches a runtime of its own version.

## Updating

1. Update every `microsandbox*` revision in the root `Cargo.toml` together, and regenerate `Cargo.lock`.
2. For a runtime release, replace the digest table in [`client.rs`](../microsandbox/src/client.rs) with the digests
   from the release's `checksums.sha256`, or, [before the release](#before-the-release), from the fork's checks. A
   source tag keeps the runtime version and digests unchanged.
3. Find any other reference to the previous pin:

   ```sh
   git grep -n -e '<previous-version>' -e '<previous-revision>'
   git grep -n -F -f <(printf '%s\n' <previous-bundle-digests>)
   ```

4. Run `make check`.
5. Start a Sandbox from an empty Microsandbox home, so a runtime downloaded earlier cannot hide a wrong release or
   digest, and from a home the previous pin has used, so the database migrations run.

Most pin updates change nothing Agent users notice and take the `skip-changelog` label. Otherwise add a changelog entry
as [CONTRIBUTING.md](../../CONTRIBUTING.md#pull-requests) says.

## Before the release

A pin update can be tested while the fork's synchronization is still in review, so that a fix it needs goes into
that synchronization rather than into another release. The fork's checks build the release assets of every tree they
test, and its release later publishes exactly those files, so their digests are final before the release exists.

1. Pin the commit the fork's checks passed on, such as the tip of its `sync/digdir-X.Y.Z` branch. Landing the
   synchronization keeps that commit, so the pin stays valid.
2. Download its release assets, which prints each bundle's digest for the digest table:

   ```sh
   sandbox/microsandbox/fetch-runtime.sh <directory>
   ```

3. Set `AGENT_MICROSANDBOX_RUNTIME_DIR=<directory>` wherever the tests or `agentctl` run. A Sandbox Provider then
   installs the runtime from that directory instead of downloading the release, and only a bundle with the pinned
   digest is accepted. The pull request's end-to-end tests do this themselves while the pinned version has no
   release.

If the synchronization changes, pin its new commit and download again. Once the release is published, rerun the pull
request's checks, which then install the published runtime, and merge it.

## Rollback

Roll back the revision, `Cargo.lock` and the digest table together.

A rollback only helps a Microsandbox home the newer pin has not opened yet. Microsandbox migrates the home's database
when it opens it, and an older version refuses a database with migrations it does not know.
