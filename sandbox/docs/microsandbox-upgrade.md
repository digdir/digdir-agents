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
   from the release's `checksums.sha256`. A source tag keeps the runtime version and digests unchanged.
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

## Rollback

Roll back the revision, `Cargo.lock` and the digest table together.

A rollback only helps a Microsandbox home the newer pin has not opened yet. Microsandbox migrates the home's database
when it opens it, and an older version refuses a database with migrations it does not know.
