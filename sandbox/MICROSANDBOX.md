# Microsandbox pin

`sandbox-microsandbox` builds on the Digdir fork of Microsandbox,
[digdir/microsandbox](https://github.com/digdir/microsandbox), and on the host runtime published with each of its
releases. How the fork is synchronized with upstream and how runtimes are released is in its
[MAINTAINING-digdir.md](https://github.com/digdir/microsandbox/blob/main-digdir/MAINTAINING-digdir.md). This document
covers the pin in this repository.

## What is pinned

- The `microsandbox*` Git dependencies in the root `Cargo.toml`, all at the same revision, and their resolution in
  `Cargo.lock`.
- The SHA-256 digest of every supported host runtime bundle, in the digest table in
  `microsandbox/src/client.rs`.

The SDK only launches a runtime of exactly its own version, so the source revision and the runtime digests always
come from the same Digdir release. Pin only tagged revisions: a `v<version>` runtime release tag, or a
`v<runtime-version>-source.<n>` tag that reuses an already published runtime. The tags keep the pinned commits
reachable after the fork's `main-digdir` is rewritten.

Consumers of the sandbox crates, such as Altinn Studio's GitHub runner coordinator, pin a revision of this
repository and download the matching runtime bundle themselves.

## Updating

Update only to a runtime release that is complete and verified, or to a source tag whose reuse of the runtime is
recorded in the fork.

1. Update every `microsandbox*` revision in the root `Cargo.toml` together, and regenerate `Cargo.lock`. Every
   Git-sourced Microsandbox package must resolve to the same revision and version.
2. For a runtime release, replace the digest table in `microsandbox/src/client.rs` with the digests from the
   release's `checksums.sha256`. A source tag keeps the runtime version and digests unchanged.
3. Search for every remaining reference to the previous pin, rather than relying on a list of files:

   ```sh
   git grep -n -e '<previous-version>' -e '<previous-revision>'
   git grep -n -F -f <(printf '%s\n' <previous-bundle-digests>)
   ```

4. Run `make fmt lint build test`, and `make test-e2e` on a host with Docker, Internet access, hardware
   virtualization, Node.js, tmux and util-linux `script`.
5. Exercise a first-run runtime installation from an empty provider home, so that stale local artifacts cannot hide
   a release or checksum error, and an upgrade from a provider home and database populated by the previous pin,
   where migrations run.

A pin update is internal maintenance unless it changes behavior Agent users can see. Use the `skip-changelog` label
for internal-only updates; otherwise describe the effect under `Unreleased` in `agentctl/CHANGELOG.md`.

## Rollback

Roll back by restoring the previous tagged revision, `Cargo.lock` resolution and digest table together. Never combine
source from one Digdir version with runtime artifacts from another.
