#!/usr/bin/env bash
# third-party-notices.sh — write the third-party notices for an agentctl release.
#
# Usage:
#   agentctl/notices/third-party-notices.sh <output-file>
#
# Covers the Rust dependencies of the `agentctl` and `agentd` binaries for every
# release target, followed by the license files the dependencies ship. Requires
# cargo-about and Python 3.

set -euo pipefail

if (( $# != 1 )); then
  echo "Usage: $0 <output-file>" >&2
  exit 2
fi

output=$(realpath -m -- "$1")
cd "$(git rev-parse --show-toplevel)"

config=agentctl/notices/about.toml
template=agentctl/notices/about.hbs
about_args=(
  --manifest-path agentctl/Cargo.toml
  --target x86_64-unknown-linux-gnu --target aarch64-unknown-linux-gnu
  --target aarch64-apple-darwin
  --target x86_64-pc-windows-msvc --target aarch64-pc-windows-msvc
)
reports=$(mktemp -d)
trap 'rm -rf "$reports"' EXIT
cargo about generate --locked --config "$config" --format json "${about_args[@]}" > "$reports/agent.json"

{
  cat <<'NOTICE'
# Third-party notices

This release of agentctl contains the `agentctl` and `agentd` binaries. agentctl
is licensed under MIT; see `LICENSE`. This file lists the third-party components
the binaries contain and their licenses.

## Rust dependencies

NOTICE
  cargo about generate --locked --config "$config" "${about_args[@]}" "$template"
  cat <<'NOTICE'

## License files shipped by the dependencies

The license texts above are identified automatically and do not always reproduce
the copyright notices of each dependency. The license, copying and notice files
that the dependencies ship are reproduced here verbatim; identical files are
listed once.

NOTICE
  python3 agentctl/notices/license-files.py "$reports/agent.json"
} > "$output"

echo "Wrote $output"
