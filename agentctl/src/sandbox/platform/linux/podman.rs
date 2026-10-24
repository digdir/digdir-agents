//! Podman's configuration: the mediated CA, the runtime, registries and the shared socket.

use ::sandbox::{SandboxHandle, SandboxPath, execution::ExecutionSpec};

use crate::Error;

use super::{super::files::write_if_changed, run_checked, wait_for_systemd};

const PODMAN: &str = "/usr/bin/podman";
// Podman reads these files when it creates containers. The default mount also
// reaches Buildah RUN containers and exposes the guest's superset bundle at a
// path no distro package owns. Distro trust paths are populated by an OCI hook
// that copies the bundle into the container root filesystem: a bind mount
// there would make the file a mount point, and package managers replacing the
// bundle (`apt-get install ca-certificates`) then fail with EBUSY. The hook
// also drops the mediator CA as an anchor into the distro's source directory
// so a regenerated bundle keeps trusting mediation. This is fail-open
// convenience; mediated networking remains the enforcement boundary if a
// workload bypasses the configuration.
const PODMAN_CONTAINERS_CONF: &str = "/etc/containers/containers.conf.d/50-agent-ca.conf";
const PODMAN_RUNTIME_CONF: &str = "/etc/containers/containers.conf.d/51-agent-runtime.conf";
const PODMAN_MOUNTS_CONF: &str = "/etc/containers/mounts.conf";
const PODMAN_REGISTRIES_CONF: &str = "/etc/containers/registries.conf.d/50-agent-docker-hub.conf";
const PODMAN_SOCKET_DROP_IN: &str = "/etc/systemd/system/podman.socket.d/50-agent-access.conf";
const PODMAN_HOOKS_DIR: &str = "/etc/containers/oci/hooks.d";
const PODMAN_CA_HOOK_CONF: &str = "/etc/containers/oci/hooks.d/50-agent-ca.json";
const PODMAN_CA_HOOK: &str = "/usr/local/libexec/agent-container-ca";
const PODMAN_USER_CONF_DIRECTORY: &str = "/home/agent/.config/containers/containers.conf.d";
const PODMAN_USER_CONF: &str = "/home/agent/.config/containers/containers.conf.d/50-agent-socket.conf";
const PODMAN_USER_CONF_CONTENTS: &[u8] = b"[engine]\nremote = true\nactive_service = \"agent\"\n\n[engine.service_destinations.agent]\nuri = \"unix:///run/podman/podman.sock\"\n";
const PODMAN_CONTAINERS_CONF_CONTENTS: &[u8] = br#"[containers]
env = [
  "SSL_CERT_FILE=/run/agent/tls/ca-bundle.pem",
  "CURL_CA_BUNDLE=/run/agent/tls/ca-bundle.pem",
  "REQUESTS_CA_BUNDLE=/run/agent/tls/ca-bundle.pem",
  "NODE_EXTRA_CA_CERTS=/run/agent/tls/ca-bundle.pem",
  "GIT_SSL_CAINFO=/run/agent/tls/ca-bundle.pem",
  "NPM_CONFIG_CAFILE=/run/agent/tls/ca-bundle.pem",
]
"#;
// The minimal systemd guest has no D-Bus system bus. Podman's default systemd
// cgroup manager therefore made crun fail with `cannot open sd-bus`; cgroupfs
// keeps ownership inside Podman instead of relying on unavailable systemd APIs.
// Sandbox teardown owns final cleanup, rather than systemd tracking these
// container cgroups as units.
// The compatibility API must apply Docker's implicit docker.io resolution as
// well; it does not consult registries.conf for that behavior.
// Implicit hook directories are deprecated, so the directory is named explicitly.
const PODMAN_RUNTIME_CONF_CONTENTS: &[u8] = b"[engine]\ncgroup_manager = \"cgroupfs\"\ncompat_api_enforce_docker_hub = true\nhooks_dir = [\"/etc/containers/oci/hooks.d\"]\n";
const PODMAN_MOUNTS_CONF_CONTENTS: &[u8] = b"/etc/ssl/certs/ca-certificates.crt:/run/agent/tls/ca-bundle.pem\n";
const PODMAN_CA_HOOK_CONF_CONTENTS: &[u8] = br#"{"version":"1.0.0","hook":{"path":"/usr/local/libexec/agent-container-ca"},"when":{"always":true},"stages":["createRuntime"]}
"#;
// Runs as an OCI `createRuntime` hook with the container state on stdin and
// the root filesystem mounted. It must not depend on tools the guest image may
// lack, so it is POSIX sh plus sed. Failures are swallowed: trust wiring is a
// convenience and must never stop a container from starting.
const PODMAN_CA_HOOK_CONTENTS: &[u8] = br#"#!/bin/sh
# Installed by agentd. Copies the mediated CA bundle into distro trust paths of a
# starting container and adds the mediator CA as an anchor for bundle regeneration.
set -u
bundle_dir=$(cat | tr -d '\n' | sed -n 's/.*"bundle"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
[ -n "$bundle_dir" ] && [ -f "$bundle_dir/config.json" ] || exit 0
rootfs=$(tr -d '\n' <"$bundle_dir/config.json" \
    | sed -n 's/.*"root"[[:space:]]*:[[:space:]]*{[^}]*"path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
[ -n "$rootfs" ] || exit 0
case "$rootfs" in /*) ;; *) rootfs="$bundle_dir/$rootfs" ;; esac
[ -d "$rootfs" ] || exit 0
bundle=/etc/ssl/certs/ca-certificates.crt
anchor=/.msb/tls/ca.pem
[ -f "$bundle" ] || exit 0
install_copy() {
    rm -f "$2" 2>/dev/null
    cp "$1" "$2" 2>/dev/null && chmod 0644 "$2" 2>/dev/null
}
for target in etc/ssl/certs/ca-certificates.crt etc/pki/tls/certs/ca-bundle.crt etc/ssl/cert.pem; do
    directory="$rootfs/${target%/*}"
    [ -d "$directory" ] || continue
    if [ "$target" = etc/ssl/certs/ca-certificates.crt ] || [ -e "$rootfs/$target" ] || [ -L "$rootfs/$target" ]; then
        install_copy "$bundle" "$rootfs/$target"
    fi
done
if [ -f "$anchor" ]; then
    if [ -d "$rootfs/usr/local/share" ]; then
        mkdir -p "$rootfs/usr/local/share/ca-certificates" 2>/dev/null \
            && install_copy "$anchor" "$rootfs/usr/local/share/ca-certificates/agent-mediator.crt"
    fi
    if [ -d "$rootfs/etc/pki/ca-trust/source/anchors" ]; then
        install_copy "$anchor" "$rootfs/etc/pki/ca-trust/source/anchors/agent-mediator.crt"
    fi
fi
exit 0
"#;
// One search registry is deterministic in enforcing mode and reproduces
// Docker's implicit docker.io[/library] normalization without alias upkeep.
const PODMAN_REGISTRIES_CONF_CONTENTS: &[u8] =
    b"unqualified-search-registries = [\"docker.io\"]\nshort-name-mode = \"enforcing\"\n";
const PODMAN_SOCKET_DROP_IN_CONTENTS: &[u8] = b"[Socket]\nDirectoryMode=0755\nSocketGroup=agent\nSocketMode=0660\n";

/// Returns whether the image has Podman.
pub(super) async fn configure_podman(sandbox: &SandboxHandle) -> Result<bool, Error> {
    let present = sandbox
        .run_execution(ExecutionSpec::command(
            SandboxPath::new("/usr/bin/test"),
            ["-x".into(), PODMAN.into()],
        ))
        .await?;
    match present.status.code {
        1 => return Ok(false),
        0 => {}
        code => {
            return Err(Error::SandboxSetup(format!(
                "Podman presence check exited with code {code}"
            )));
        }
    }

    wait_for_systemd(sandbox).await?;
    run_checked(
        sandbox,
        "/usr/bin/sudo",
        [
            "-n",
            "/usr/bin/install",
            "-d",
            "-m",
            "0755",
            "/etc/containers/containers.conf.d",
            "/etc/containers/registries.conf.d",
            "/etc/systemd/system/podman.socket.d",
            PODMAN_HOOKS_DIR,
            "/usr/local/libexec",
        ],
    )
    .await?;
    write_if_changed(sandbox, PODMAN_CONTAINERS_CONF, PODMAN_CONTAINERS_CONF_CONTENTS).await?;
    write_if_changed(sandbox, PODMAN_RUNTIME_CONF, PODMAN_RUNTIME_CONF_CONTENTS).await?;
    write_if_changed(sandbox, PODMAN_MOUNTS_CONF, PODMAN_MOUNTS_CONF_CONTENTS).await?;
    write_if_changed(sandbox, PODMAN_REGISTRIES_CONF, PODMAN_REGISTRIES_CONF_CONTENTS).await?;
    write_if_changed(sandbox, PODMAN_SOCKET_DROP_IN, PODMAN_SOCKET_DROP_IN_CONTENTS).await?;
    write_if_changed(sandbox, PODMAN_CA_HOOK_CONF, PODMAN_CA_HOOK_CONF_CONTENTS).await?;
    write_if_changed(sandbox, PODMAN_CA_HOOK, PODMAN_CA_HOOK_CONTENTS).await?;
    run_checked(sandbox, "/usr/bin/sudo", ["-n", "/bin/chmod", "0755", PODMAN_CA_HOOK]).await?;
    run_checked(
        sandbox,
        "/usr/bin/sudo",
        ["-n", "/usr/bin/install", "-d", "-m", "0755", "/run/podman"],
    )
    .await?;
    run_checked(sandbox, "/usr/bin/sudo", ["-n", "/usr/bin/systemctl", "daemon-reload"]).await?;
    // An already-listening socket retains its old mode until the next Sandbox
    // boot; the compile-time drop-in is not changed independently at runtime.
    run_checked(
        sandbox,
        "/usr/bin/sudo",
        ["-n", "/usr/bin/systemctl", "enable", "--now", "podman.socket"],
    )
    .await?;
    Ok(true)
}

/// Makes every `podman` the agent user runs a client of the shared root Podman.
pub(super) async fn point_podman_at_socket(sandbox: &SandboxHandle) -> Result<(), Error> {
    run_checked(sandbox, "/usr/bin/mkdir", ["-p", PODMAN_USER_CONF_DIRECTORY]).await?;
    write_if_changed(sandbox, PODMAN_USER_CONF, PODMAN_USER_CONF_CONTENTS).await?;
    run_checked(
        sandbox,
        "/usr/bin/sudo",
        ["-n", "/usr/bin/chown", "agent:agent", PODMAN_USER_CONF],
    )
    .await
}
