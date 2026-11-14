//! Declarative agent manifest and observed resource status.

use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

use time::OffsetDateTime;

use ::sandbox::{
    ByteQuantity, CpuQuantity, Platform, RetentionPolicy, RootFilesystem, SandboxName, SandboxPath, SandboxResources,
    image::ImageSource, init::InitSystem,
};
use serde::{Deserialize, Serialize};

use crate::{Error, HarnessSpec, harness};

/// The first supported agent manifest API version.
pub const API_VERSION: &str = "agents.platform/v1alpha1";
/// The manifest resource kind.
pub const KIND: &str = "Agent";
/// Manifest kind used for a partial agent configuration.
pub const VARIANT_KIND: &str = "AgentVariant";
/// Maximum number of manifests in one inheritance chain, including the complete agent.
pub const MAX_VARIANT_CHAIN: usize = 16;

/// Declarative resource accepted by the agent control plane.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Agent {
    /// Manifest schema version.
    pub api_version: String,
    /// Resource kind.
    pub kind: String,
    /// Resource identity and API-managed metadata.
    pub metadata: Metadata,
    /// Desired agent and sandbox configuration.
    pub spec: Spec,
    /// Most recently observed state.
    #[serde(default, skip_serializing_if = "Status::is_empty")]
    pub status: Status,
}

/// A partial agent manifest that inherits from a sibling manifest.
///
/// `metadata` and `spec` remain YAML values until they have been merged with a
/// complete agent. The expanded document is then decoded through [`Agent`], so
/// nested unknown fields are rejected by the same strict contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentVariant {
    /// Manifest schema version, which must match every manifest in the chain.
    pub api_version: String,
    /// Resource kind. Always [`VARIANT_KIND`].
    pub kind: String,
    /// Sibling manifest filename inherited by this variant.
    pub extends: String,
    /// Partial resource metadata. `name` is required in every variant.
    pub metadata: serde_yaml_ng::Value,
    /// Partial desired agent configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<serde_yaml_ng::Value>,
}

/// Validated selector identifying an `agent.<variant>.yaml` leaf.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentVariantName(String);

impl AgentVariantName {
    /// Creates a validated agent variant name.
    ///
    /// # Errors
    ///
    /// Returns an error unless the name matches `[a-z0-9]+(?:-[a-z0-9]+)*`.
    pub fn new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.is_empty()
            || !value.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            })
        {
            return Err(Error::Invalid("variant must match [a-z0-9]+(?:-[a-z0-9]+)*".into()));
        }
        Ok(Self(value))
    }

    /// Returns the selector text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the conventional leaf filename for this variant.
    #[must_use]
    pub fn filename(&self) -> String {
        format!("agent.{self}.yaml")
    }
}

impl TryFrom<String> for AgentVariantName {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<AgentVariantName> for String {
    fn from(value: AgentVariantName) -> Self {
        value.0
    }
}

impl std::str::FromStr for AgentVariantName {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl std::fmt::Display for AgentVariantName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for AgentVariantName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// A leaf manifest expanded to the complete agent sent to the control plane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedManifest {
    /// Expanded and strictly validated agent.
    pub agent: Agent,
    /// Manifest paths from the leaf through to the complete base agent.
    pub chain: Vec<PathBuf>,
}

impl Agent {
    /// Validates fields required at every API boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when the resource version, kind, name, or sandbox specification is invalid.
    pub fn validate(&self) -> Result<(), Error> {
        if self.api_version != API_VERSION {
            return Err(Error::Invalid(format!("apiVersion must be {API_VERSION:?}")));
        }
        if self.kind != KIND {
            return Err(Error::Invalid(format!("kind must be {KIND:?}")));
        }
        SandboxName::new(self.metadata.name.clone())
            .map_err(|error| Error::Invalid(format!("metadata.name: {error}")))?;
        self.spec
            .sandbox
            .validate()
            .map_err(|error| Error::Invalid(format!("spec.sandbox: {error}")))?;
        self.spec.validate()
    }

    pub(crate) fn clear_managed_fields(&mut self) {
        self.metadata.generation = 0;
        self.metadata.deletion_timestamp = None;
        self.metadata.restart_timestamp = None;
        self.status = Status::default();
    }

    /// Returns whether a restart was asked for that no pass has carried out yet.
    #[must_use]
    pub fn restart_pending(&self) -> bool {
        self.metadata.restart_timestamp.is_some()
            && self.metadata.restart_timestamp != self.status.observed_restart_timestamp
    }
}

/// Agent resource identity and API-managed metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Metadata {
    /// Stable resource name.
    pub name: String,
    /// Desired-state revision managed by the control plane.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub generation: u64,
    /// Time at which asynchronous deletion was requested.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub deletion_timestamp: Option<OffsetDateTime>,
    /// Time of the latest restart asked for with `agentctl restart`. The reconciler stops and
    /// starts the sandbox until its status has observed this restart.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub restart_timestamp: Option<OffsetDateTime>,
}

/// Desired agent settings and exactly one generic sandbox specification.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Spec {
    /// Whether the agent's sandbox runs. `agentctl stop`, `agentctl start` and
    /// `agentctl restart` change it. A manifest may set it; when it is omitted, `apply` keeps the
    /// agent's current run state, and a new agent runs. Stored only when not
    /// Running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_state: Option<RunState>,
    /// Generic sandbox configuration mapped to the lower-layer SDK.
    pub sandbox: SandboxManifestSpec,
    /// Host directory synchronized into the sandbox user's home at bootstrap, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<HomeSpec>,
    /// Where sessions start, such as the folder a bind mount puts the agent's checkout in. The
    /// sandbox's working directory when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<SandboxPath>,
    /// Agent-wide guidance installed through every declared Harness Adapter, concatenated in order.
    #[serde(default, deserialize_with = "null_as_default", skip_serializing_if = "Vec::is_empty")]
    pub instructions: Vec<InstructionsSpec>,
    /// Skill directories installed through every declared Harness Adapter.
    #[serde(default, deserialize_with = "null_as_default", skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<SkillSpec>,
    /// Harness installations available to sessions in this agent. An agent without one runs no
    /// sessions, only the commands run in it.
    #[serde(default, deserialize_with = "null_as_default")]
    pub harnesses: Vec<HarnessSpec>,
    /// Deliberately selected non-secret values exposed in plaintext inside the sandbox.
    #[serde(default, deserialize_with = "null_as_default", skip_serializing_if = "Vec::is_empty")]
    pub environment: Vec<EnvironmentSpec>,
    /// Host-owned values made available only through mediated requests.
    #[serde(default, deserialize_with = "null_as_default", skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<SecretSpec>,
    /// Ways the agent's user reaches into the sandbox besides sessions and `exec`.
    #[serde(default, deserialize_with = "null_as_default", skip_serializing_if = "Vec::is_empty")]
    pub access: Vec<AccessSpec>,
    /// Accounts the Agent acts as, each signed in once on the host with `agentctl`.
    #[serde(default, deserialize_with = "null_as_default", skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<AccountSpec>,
    /// Sandbox egress mediation policy; mediated with every host allowed when omitted.
    #[serde(default, deserialize_with = "null_as_default")]
    pub network: NetworkSpec,
}

/// Whether an agent's sandbox runs.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum RunState {
    /// The sandbox runs and serves sessions and Executions.
    #[default]
    Running,
    /// The sandbox's VM is stopped. Its root filesystem, Volumes and identity
    /// are kept, so a start boots the same disk and sessions resume.
    Stopped,
}

/// One access capability the platform provides to the agent's user.
///
/// Access is an agent-level capability like `harnesses` and `secrets`: the
/// platform owns the guest user, the transport and the key material, so a
/// variant carries no tunables.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase", tag = "type")]
pub enum AccessSpec {
    /// OpenSSH access as the platform-owned guest user, reached through `agentctl ssh`.
    ///
    /// A struct variant so that `deny_unknown_fields` rejects tunables; serde
    /// does not enforce it for unit variants of an internally tagged enum.
    Ssh {},
    /// VNC access to the desktop the image runs, reached through `agentctl vnc`.
    Vnc {},
}

/// One account the agent acts as.
///
/// The platform owns the credential, its placeholder and the hosts it is substituted for; a
/// manifest only says which account.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase", tag = "type")]
pub enum AccountSpec {
    /// The GitHub account behind the login `agentctl github login <login>` kept: a mediated
    /// `GITHUB_TOKEN` for GitHub's hosts, and the account as Git identity.
    Github {
        /// The login the agent uses, as it was kept, such as `altinn-studio`.
        login: String,
    },
}

impl AccountSpec {
    /// The account's type.
    #[must_use]
    pub const fn account_type(&self) -> crate::accounts::AccountType {
        match self {
            Self::Github { .. } => crate::accounts::AccountType::Github,
        }
    }

    /// The login the agent uses for the account, by the name it was kept under.
    #[must_use]
    pub fn login(&self) -> &str {
        match self {
            Self::Github { login } => login,
        }
    }
}

/// Sandbox settings as supplied by an agent manifest.
///
/// Unlike the lower-layer [`sandbox::SandboxSpec`], this representation retains
/// an omitted architecture until the agent creates the concrete sandbox request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxManifestSpec {
    /// Source of the immutable Image.
    pub image: ImageSource,
    /// Desired platform constraints.
    pub platform: PlatformManifestSpec,
    /// Desired mutable compute and writable root filesystem resources.
    pub resources: ResourcesManifestSpec,
    /// Process responsible for initializing the sandbox after backend setup.
    #[serde(default, deserialize_with = "null_as_default")]
    pub init_system: InitSystem,
    /// Whether the agent retains the sandbox when releasing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_policy: Option<RetentionPolicy>,
    /// Host filesystem and in-memory attachments materialized with the sandbox.
    #[serde(default, deserialize_with = "null_as_default", skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<MountSpec>,
}

/// Sandbox resources as supplied by an agent manifest.
///
/// Unlike the lower-layer [`SandboxResources`], which turns nested sandboxes on or off, this
/// representation keeps how much the agent needs them; the agent decides from that and the host.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResourcesManifestSpec {
    /// Desired CPU quantity.
    pub cpu: CpuQuantity,
    /// Desired sandbox memory quantity.
    pub memory: ByteQuantity,
    /// Desired writable root filesystem capacity and mode.
    pub root_filesystem: RootFilesystem,
    /// Whether the sandbox runs sandboxes of its own, such as nested agents. Absent, it does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nested_sandboxes: Option<NestedSandboxes>,
}

impl ResourcesManifestSpec {
    /// Returns the sandbox resources without nested sandboxes, which the agent adds when it has
    /// decided them.
    #[must_use]
    pub const fn sandbox_resources(self) -> SandboxResources {
        SandboxResources::new(self.cpu, self.memory, self.root_filesystem)
    }
}

/// How much an agent needs its sandbox to run sandboxes of its own.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NestedSandboxes {
    /// The agent cannot do without them: it fails on a host that cannot provide them.
    Required,
    /// The agent uses them where the host provides them, and runs without them elsewhere.
    Preferred,
}

impl SandboxManifestSpec {
    fn validate(&self) -> Result<(), sandbox::Error> {
        self.image.validate()?;
        self.platform.validate()
    }

    /// Resolves manifest-relative sources and an omitted architecture for a concrete Provider request.
    ///
    /// # Errors
    ///
    /// Returns an error when the build context starts with `~` and the home directory is unknown.
    pub fn resolve_from(
        &self,
        source_directory: &std::path::Path,
        default_architecture: &str,
    ) -> Result<sandbox::SandboxSpec, Error> {
        let mut image = self.image.clone();
        if let ImageSource::Build { context, .. } = &mut image {
            *context = resolve_host_path(source_directory, context, "spec.sandbox.image.context")?;
        }
        Ok(sandbox::SandboxSpec {
            image,
            platform: self.platform.resolve(default_architecture),
            resources: self.resources.sandbox_resources(),
            init_system: self.init_system,
            retention_policy: self.resolved_retention_policy(),
        })
    }

    /// Returns the agent-layer retention default used when the manifest omits it.
    #[must_use]
    pub fn resolved_retention_policy(&self) -> RetentionPolicy {
        self.retention_policy.unwrap_or(RetentionPolicy::Delete)
    }
}

/// One filesystem attachment declared by an agent builder.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum MountSpec {
    /// A caller-host directory mapped into the sandbox.
    Bind {
        /// Host path, resolved relative to the manifest directory at apply time. Whoever applies or
        /// runs the agent can give another, as with `--mount SOURCE:TARGET`. Without one, they must:
        /// the definition leaves the folder to them, as one that works on any checkout does.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<std::path::PathBuf>,
        /// Absolute path inside the sandbox.
        target: SandboxPath,
        /// Whether the sandbox may modify the host directory.
        read_only: bool,
        /// Whether a missing source leaves the mount out rather than failing apply. A sandbox's mounts cannot
        /// change, so whether the source exists is decided when the agent is created: a source that appears or
        /// disappears later changes nothing until the agent is recreated.
        #[serde(default, deserialize_with = "null_as_default", skip_serializing_if = "is_false")]
        optional: bool,
    },
    /// A volume, by name. The agent keeps the revision it gets first, and waits until the volume has a revision
    /// ready.
    Volume {
        /// Name of the volume.
        volume: String,
        /// Absolute path inside the sandbox.
        target: SandboxPath,
        /// Whether the sandbox may not change the files. A copy never changes, so the changes a writable mount makes
        /// stay in the agent, on its root filesystem, and are lost when the agent is recreated.
        read_only: bool,
        /// Whether a volume that does not exist, or whose copies failed, leaves the mount out rather than
        /// making the agent wait. As for a bind mount, this is decided when the agent gets its revisions:
        /// a volume applied later is mounted once the agent is recreated. A volume taking its first copy
        /// is still waited for.
        #[serde(default, deserialize_with = "null_as_default", skip_serializing_if = "is_false")]
        optional: bool,
    },
    /// Anonymous in-memory storage with an explicit capacity.
    Tmpfs {
        /// Absolute path inside the sandbox.
        target: SandboxPath,
        /// Maximum storage capacity.
        capacity: ByteQuantity,
    },
}

impl MountSpec {
    const fn target(&self) -> &SandboxPath {
        match self {
            Self::Bind { target, .. } | Self::Volume { target, .. } | Self::Tmpfs { target, .. } => target,
        }
    }
}

fn validate_mounts(mounts: &[MountSpec], root_filesystem: ::sandbox::RootFilesystemMode) -> Result<(), Error> {
    let mut targets = std::collections::BTreeSet::new();
    for (index, mount) in mounts.iter().enumerate() {
        if let MountSpec::Bind { source, optional, .. } = mount {
            validate_bind_mount(index, source.as_deref(), *optional)?;
        }
        if let MountSpec::Volume {
            volume,
            target,
            read_only,
            ..
        } = mount
        {
            validate_volume_mount(index, volume, target, *read_only)?;
            // A writable layer's changes cannot be kept on an overlay, which a layered root filesystem is.
            if !read_only && root_filesystem == ::sandbox::RootFilesystemMode::Layered {
                return Err(Error::Invalid(format!(
                    "spec.sandbox.mounts[{index}].readOnly: false needs spec.sandbox.resources.rootFilesystem.mode: \
                     direct, as a layered root filesystem cannot keep the changes"
                )));
            }
        }
        let target = mount.target().as_str();
        if !valid_sandbox_path(target) || !targets.insert(target) {
            return Err(Error::Invalid(format!(
                "spec.sandbox.mounts[{index}].target must be a unique absolute normalized sandbox path"
            )));
        }
    }
    reject_mounts_inside_writable_volumes(mounts)
}

fn validate_bind_mount(index: usize, source: Option<&std::path::Path>, optional: bool) -> Result<(), Error> {
    let field = format!("spec.sandbox.mounts[{index}]");
    match source {
        Some(source) if source.as_os_str().is_empty() => {
            Err(Error::Invalid(format!("{field}.source must not be empty")))
        }
        None if optional => Err(Error::Invalid(format!(
            "{field} is optional, which needs a source to leave out when it is missing"
        ))),
        _ => Ok(()),
    }
}

/// A writable volume's layer is mounted over its target after the other mounts, so it would hide
/// any of them inside the target.
fn reject_mounts_inside_writable_volumes(mounts: &[MountSpec]) -> Result<(), Error> {
    let writable = mounts.iter().enumerate().filter_map(|(index, mount)| match mount {
        MountSpec::Volume {
            target,
            read_only: false,
            ..
        } => Some((index, target.as_str())),
        _ => None,
    });
    for (index, writable) in writable {
        if let Some(inner) = mounts.iter().position(|mount| {
            mount
                .target()
                .as_str()
                .strip_prefix(writable)
                .is_some_and(|rest| rest.starts_with('/'))
        }) {
            return Err(Error::Invalid(format!(
                "spec.sandbox.mounts[{inner}].target is inside writable volume mount \
                 spec.sandbox.mounts[{index}], which would hide it; mount it elsewhere or make the volume read-only"
            )));
        }
    }
    Ok(())
}

fn validate_volume_mount(index: usize, volume: &str, target: &SandboxPath, read_only: bool) -> Result<(), Error> {
    SandboxName::new(volume.to_owned())
        .map_err(|error| Error::Invalid(format!("spec.sandbox.mounts[{index}].volume: {error}")))?;
    // These separate the options the writable layer is mounted with.
    if !read_only && target.as_str().contains([',', ':', '\\']) {
        return Err(Error::Invalid(format!(
            "spec.sandbox.mounts[{index}].target of a writable volume must not contain `,`, `:` or `\\`"
        )));
    }
    Ok(())
}

/// Platform constraints retained exactly as supplied by an agent manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlatformManifestSpec {
    /// Operating system, such as `linux`.
    pub os: String,
    /// Optional CPU architecture; omission selects the provider's native architecture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    /// Optional architecture variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// Optional operating-system version constraint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    /// Required operating-system features.
    #[serde(
        default,
        deserialize_with = "null_as_default",
        skip_serializing_if = "std::collections::BTreeSet::is_empty"
    )]
    pub os_features: std::collections::BTreeSet<String>,
}

impl PlatformManifestSpec {
    fn validate(&self) -> Result<(), sandbox::Error> {
        if self.os.is_empty() {
            return Err(sandbox::Error::invalid("platform.os", "must not be empty"));
        }
        if self.architecture.as_ref().is_some_and(String::is_empty) {
            return Err(sandbox::Error::invalid(
                "platform.architecture",
                "must not be empty when present",
            ));
        }
        if self.variant.as_ref().is_some_and(String::is_empty) {
            return Err(sandbox::Error::invalid(
                "platform.variant",
                "must not be empty when present",
            ));
        }
        if self.os_version.as_ref().is_some_and(String::is_empty) {
            return Err(sandbox::Error::invalid(
                "platform.osVersion",
                "must not be empty when present",
            ));
        }
        if self.os_features.contains("") {
            return Err(sandbox::Error::invalid(
                "platform.osFeatures",
                "must not contain an empty value",
            ));
        }
        Ok(())
    }

    fn resolve(&self, default_architecture: &str) -> Platform {
        Platform {
            os: self.os.clone(),
            architecture: self.architecture.clone().unwrap_or_else(|| default_architecture.into()),
            variant: self.variant.clone(),
            os_version: self.os_version.clone(),
            os_features: self.os_features.clone(),
        }
    }
}

impl Spec {
    /// Returns the desired run state; an omitted one is Running.
    #[must_use]
    pub fn run_state(&self) -> RunState {
        self.run_state.unwrap_or_default()
    }

    /// Returns whether the agent's sandbox is meant to be stopped.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.run_state() == RunState::Stopped
    }

    /// Sets the desired run state in its stored form, which omits Running, so
    /// specs that differ only in how they say Running compare equal.
    pub(crate) fn set_run_state(&mut self, state: RunState) {
        self.run_state = (state != RunState::Running).then_some(state);
    }

    /// Returns the declared installation for `kind`.
    #[must_use]
    pub fn harness(&self, kind: crate::Harness) -> Option<&HarnessSpec> {
        self.harnesses.iter().find(|harness| harness.kind == kind)
    }

    /// Returns the installation selected for a new session without an explicit harness.
    #[must_use]
    pub fn default_harness(&self) -> Option<&HarnessSpec> {
        if self.harnesses.len() == 1 {
            self.harnesses.first()
        } else {
            self.harnesses.iter().find(|harness| harness.default)
        }
    }

    /// Returns whether the agent declares SSH access.
    #[must_use]
    pub fn ssh_access(&self) -> bool {
        self.access.contains(&AccessSpec::Ssh {})
    }

    /// Returns whether the agent declares VNC access.
    #[must_use]
    pub fn vnc_access(&self) -> bool {
        self.access.contains(&AccessSpec::Vnc {})
    }

    /// The variables read from the environment file, which a later release stops reading: those of
    /// `spec.environment` without a `value` and of `spec.secrets` without a `name`. Each is named as
    /// the file names it, followed by the variable it sets when that differs, such as
    /// `HOST_EDITOR as EDITOR`.
    #[must_use]
    pub fn environment_file_variables(&self) -> Vec<String> {
        let named = |source: &str, name: &str| {
            if source == name {
                source.to_owned()
            } else {
                format!("{source} as {name}")
            }
        };
        self.environment
            .iter()
            .filter_map(|variable| variable.source().map(|source| named(source, &variable.name)))
            .chain(
                self.secrets
                    .iter()
                    .filter_map(|secret| secret.source().map(|source| named(source, &secret.environment))),
            )
            .collect()
    }

    /// Whether a secret takes its value from the agent's environment file, which then holds real
    /// values that must not reach a sandbox.
    #[must_use]
    pub fn reads_secret_file(&self) -> bool {
        self.secrets.iter().any(|secret| secret.source().is_some())
    }

    /// The login the agent uses for its account of `account_type`, when it has one.
    #[must_use]
    pub fn account(&self, account_type: crate::accounts::AccountType) -> Option<&str> {
        self.accounts
            .iter()
            .find(|account| account.account_type() == account_type)
            .map(AccountSpec::login)
    }

    fn validate(&self) -> Result<(), Error> {
        validate_mounts(&self.sandbox.mounts, self.sandbox.resources.root_filesystem.mode())?;
        if let Some(directory) = &self.working_directory
            && !valid_sandbox_path(directory.as_str())
        {
            return Err(Error::Invalid(
                "spec.workingDirectory must be an absolute normalized sandbox path".into(),
            ));
        }
        if self
            .home
            .as_ref()
            .is_some_and(|home| home.source.as_os_str().is_empty())
        {
            return Err(Error::Invalid("spec.home.source must not be empty".into()));
        }
        self.validate_instructions()?;
        self.validate_skills()?;
        let mut harness_kinds = std::collections::BTreeSet::new();
        let mut duplicate_harness = None;
        let mut default_count = 0;
        for (index, harness) in self.harnesses.iter().enumerate() {
            if harness.version.as_deref().is_some_and(str::is_empty) {
                return Err(Error::Invalid(format!(
                    "spec.harnesses[{index}].version must not be empty"
                )));
            }
            // The mode follows from the harness; an omitted one already took it, so only a
            // contradicting spelling fails.
            let required = harness.kind.auth_mode();
            if harness.auth != required {
                return Err(Error::Invalid(format!(
                    "spec.harnesses[{index}].auth must be {:?} for harness {:?}",
                    required.as_str(),
                    harness.kind.as_str()
                )));
            }
            if harness.optional && harness.auth == harness::HarnessAuthMode::None {
                return Err(Error::Invalid(format!(
                    "spec.harnesses[{index}].optional needs a harness with a platform login to wait for; \
                     declare the credentials as optional spec.secrets instead"
                )));
            }
            if !harness.login.is_default() && harness.auth == harness::HarnessAuthMode::None {
                return Err(Error::Invalid(format!(
                    "spec.harnesses[{index}].login needs a harness with a platform login; \
                     {:?} has none",
                    harness.kind.as_str()
                )));
            }
            if !harness_kinds.insert(harness.kind) {
                duplicate_harness = Some(harness.kind);
            }
            default_count += usize::from(harness.default);
        }
        if default_count > 1 || (self.harnesses.len() > 1 && default_count != 1) {
            return Err(Error::Invalid(
                "spec.harnesses must declare exactly one default when multiple harnesses are installed".into(),
            ));
        }
        if let Some(harness) = duplicate_harness {
            return Err(Error::Invalid(format!(
                "spec.harnesses contains duplicate harness kind {:?}",
                harness.as_str()
            )));
        }
        self.validate_accounts()?;
        self.validate_environment()?;
        self.validate_secrets()?;
        let mut access = std::collections::BTreeSet::new();
        if let Some(index) = self.access.iter().position(|capability| !access.insert(*capability)) {
            return Err(Error::Invalid(format!(
                "spec.access[{index}] duplicates an access capability"
            )));
        }
        if self.network.deny.iter().any(|host| !crate::host_pattern::valid(host)) {
            return Err(Error::Invalid(
                "spec.network.deny contains an invalid host pattern".into(),
            ));
        }
        if let NetworkAllow::Hosts(hosts) = &self.network.allow {
            if hosts.iter().any(|host| !crate::host_pattern::valid(host)) {
                return Err(Error::Invalid(
                    "spec.network.allow contains an invalid host pattern".into(),
                ));
            }
            // A list admits a host by the name the platform's DNS resolved, so an address never
            // matches anything.
            if let Some(address) = hosts.iter().find(|host| crate::host_pattern::is_address(host)) {
                return Err(Error::Invalid(format!(
                    "spec.network.allow names hosts, not addresses such as {address:?}: an agent reaches a listed \
                     host by its name, through its DNS"
                )));
            }
        }
        Ok(())
    }
}

impl Spec {
    /// An agent acts as at most one account of each type, so each type's credential has one
    /// placeholder and one Git identity.
    fn validate_accounts(&self) -> Result<(), Error> {
        let mut types = std::collections::BTreeSet::new();
        for (index, account) in self.accounts.iter().enumerate() {
            if !types.insert(account.account_type()) {
                return Err(Error::Invalid(format!(
                    "spec.accounts[{index}] is a second {} account; an agent has at most one of each type",
                    account.account_type()
                )));
            }
            if !crate::names::valid(account.login()) {
                return Err(Error::Invalid(format!(
                    "spec.accounts[{index}].login: {}",
                    crate::names::RULE
                )));
            }
        }
        Ok(())
    }

    fn validate_secrets(&self) -> Result<(), Error> {
        let mut environments = self
            .environment
            .iter()
            .map(|variable| variable.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let environment_sources = self
            .environment
            .iter()
            .filter_map(EnvironmentSpec::source)
            .collect::<std::collections::BTreeSet<_>>();
        let mut placeholders = std::collections::BTreeSet::new();
        for (index, secret) in self.secrets.iter().enumerate() {
            let placeholder = secret.inert_value();
            if environments.contains(secret.environment.as_str())
                || secret
                    .source()
                    .is_some_and(|source| environment_sources.contains(source))
            {
                return Err(Error::Invalid(format!(
                    "spec.environment collides with spec.secrets[{index}]"
                )));
            }
            if let Some(name) = &secret.name {
                if secret.source.is_some() {
                    return Err(Error::Invalid(format!(
                        "spec.secrets[{index}] takes its value from the stored secret {name:?}, so it has no source"
                    )));
                }
                if !crate::names::valid(name) {
                    return Err(Error::Invalid(format!(
                        "spec.secrets[{index}].name is invalid: {}",
                        crate::names::RULE
                    )));
                }
            }
            if !valid_environment_variable(&secret.environment)
                || secret
                    .source
                    .as_deref()
                    .is_some_and(|source| !valid_environment_variable(source))
                || secret.placeholder.as_ref().is_some_and(String::is_empty)
                || self.harnesses.iter().any(|installation| {
                    harness::conflicts_with_managed_secret(
                        installation.kind,
                        &secret.environment,
                        secret.placeholder.as_deref(),
                    )
                })
                || (self.account(crate::accounts::AccountType::Github).is_some()
                    && crate::github::conflicts_with_managed_secret(&secret.environment, &placeholder))
                || (secret.allowed_hosts.is_empty() && secret.name.is_none())
                || !environments.insert(&secret.environment)
                || !placeholders.insert(placeholder)
                || secret
                    .allowed_hosts
                    .iter()
                    .any(|host| !crate::host_pattern::valid(host))
            {
                return Err(Error::Invalid(format!(
                    "spec.secrets[{index}] is invalid or duplicated"
                )));
            }
        }
        Ok(())
    }

    fn validate_environment(&self) -> Result<(), Error> {
        let mut names = std::collections::BTreeSet::new();
        for (index, variable) in self.environment.iter().enumerate() {
            if !valid_environment_variable(&variable.name)
                || variable
                    .source
                    .as_deref()
                    .is_some_and(|source| !valid_environment_variable(source))
                || (variable.source.is_some() && variable.value.is_some())
                || variable.value.as_ref().is_some_and(|value| value.contains('\0'))
                || self
                    .harnesses
                    .iter()
                    .any(|installation| harness::manages_environment(installation.kind, &variable.name))
                || (self.account(crate::accounts::AccountType::Github).is_some()
                    && variable.name == crate::github::ACCESS_ENVIRONMENT)
                || !names.insert(variable.name.as_str())
            {
                return Err(Error::Invalid(format!(
                    "spec.environment[{index}] is invalid, duplicated, or managed by a declared harness or account"
                )));
            }
        }
        let has_git_name = names.contains("GIT_USER_NAME");
        let has_git_email = names.contains("GIT_USER_EMAIL");
        if has_git_name != has_git_email {
            return Err(Error::Invalid(
                "spec.environment must declare GIT_USER_NAME and GIT_USER_EMAIL together".into(),
            ));
        }
        Ok(())
    }

    /// A variant names an entry by its source as written, so no two entries may share one.
    fn validate_instructions(&self) -> Result<(), Error> {
        let mut sources = std::collections::BTreeSet::new();
        for (index, instructions) in self.instructions.iter().enumerate() {
            let source = instructions.source.as_os_str();
            if source.is_empty() {
                return Err(Error::Invalid(format!(
                    "spec.instructions[{index}].source must not be empty"
                )));
            }
            if !sources.insert(source) {
                return Err(Error::Invalid(format!(
                    "spec.instructions[{index}] duplicates source \"{}\"",
                    instructions.source.display()
                )));
            }
        }
        Ok(())
    }

    fn validate_skills(&self) -> Result<(), Error> {
        let mut skill_names = std::collections::BTreeSet::new();
        for (index, skill) in self.skills.iter().enumerate() {
            let Some(name) = skill.name() else {
                return Err(Error::Invalid(format!(
                    "spec.skills[{index}] must declare a name or use a source ending in the skill's directory name"
                )));
            };
            if !crate::harness::valid_skill_name(name) {
                return Err(Error::Invalid(format!("spec.skills[{index}].name is invalid")));
            }
            if !skill_names.insert(name) {
                return Err(Error::Invalid(format!(
                    "spec.skills[{index}] duplicates skill {name:?}"
                )));
            }
        }
        Ok(())
    }
}

/// One explicitly selected non-secret value copied from the agent environment file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EnvironmentSpec {
    /// Environment variable exposed inside the sandbox.
    pub name: String,
    /// Optional variable name in the agent environment file; defaults to `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The value itself, so the environment file supplies none. The sandbox sees it as it is, so it
    /// is never a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

impl EnvironmentSpec {
    /// Returns the environment-file variable that supplies the plaintext value, or `None` when the
    /// manifest gives the [`Self::value`].
    #[must_use]
    pub fn source(&self) -> Option<&str> {
        match self.value {
            Some(_) => None,
            None => Some(self.source.as_deref().unwrap_or(&self.name)),
        }
    }
}

fn valid_sandbox_path(path: &str) -> bool {
    path.starts_with('/')
        && path != "/"
        && path
            .split('/')
            .skip(1)
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

/// Host inputs synchronized into the sandbox user's home.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HomeSpec {
    /// Host path, resolved relative to the manifest directory.
    pub source: std::path::PathBuf,
}

/// One harness-neutral instruction file; several are concatenated in manifest order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct InstructionsSpec {
    /// Host file, resolved relative to the manifest directory.
    pub source: std::path::PathBuf,
    /// Whether a missing source is skipped rather than failing setup.
    #[serde(default, deserialize_with = "null_as_default", skip_serializing_if = "is_false")]
    pub optional: bool,
}

/// One skill directory installed for every declared harness.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SkillSpec {
    /// Host directory holding `SKILL.md`, resolved relative to the manifest directory.
    pub source: std::path::PathBuf,
    /// Installed skill directory name; defaults to the source directory name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether a missing source directory is skipped rather than failing setup. A skill installed before its
    /// source went missing stays in the agent until it is recreated.
    #[serde(default, deserialize_with = "null_as_default", skip_serializing_if = "is_false")]
    pub optional: bool,
}

impl SkillSpec {
    /// Returns the explicit skill name or the final component of the source directory.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref().or_else(|| {
            self.source
                .file_name()?
                .to_str()
                .filter(|name| !name.is_empty() && *name != ".")
        })
    }
}

/// One host-owned value exposed to sandbox processes only as an inert environment placeholder.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SecretSpec {
    /// Guest environment variable and stable secret binding name.
    pub environment: String,
    /// Whether the agent runs without it when its value is missing, rather than failing.
    #[serde(default, deserialize_with = "null_as_default", skip_serializing_if = "is_false")]
    pub optional: bool,
    /// Optional inert value; the selected Network Backend generates one when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Hosts at which this secret may be substituted. A secret stored under [`Self::name`] has
    /// hosts of its own, which these can only narrow, so it may leave them out.
    #[serde(default, deserialize_with = "null_as_default", skip_serializing_if = "Vec::is_empty")]
    pub allowed_hosts: Vec<String>,
    /// Optional variable name in the manifest directory's `.env`; defaults to `environment`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The name of a secret stored on this machine, such as with `agentctl secret set`, that holds
    /// the value instead of the environment file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
}

/// Reads `null` as the field's default, as leaving the field out does. A variant keeps the `null` it gives a field its
/// base does not set, so that a field agentctl does not know still fails decoding.
pub(crate) fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

impl SecretSpec {
    /// Returns the host `.env` variable that supplies the secret material, or `None` when a stored
    /// secret, [`Self::name`], does.
    #[must_use]
    pub fn source(&self) -> Option<&str> {
        match self.name {
            Some(_) => None,
            None => Some(self.source.as_deref().unwrap_or(&self.environment)),
        }
    }

    /// Returns the explicit or provider-neutral generated value exposed inside the sandbox.
    #[must_use]
    pub fn inert_value(&self) -> String {
        self.placeholder
            .clone()
            .unwrap_or_else(|| format!("$AGENT_SECRET_{}", self.environment))
    }
}

/// Required network mediation mode.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NetworkMode {
    /// Route sandbox traffic through the trusted mediation backend.
    #[default]
    Mediated,
}

/// Baseline egress policy, written `all` or as a list of host patterns.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum NetworkAllow {
    /// Permit network operations except explicitly denied hosts.
    #[default]
    All,
    /// Permit only the hosts one of these patterns matches, such as `api.example.com` or
    /// `*.example.com`, except denied hosts. An empty list reaches no host.
    Hosts(Vec<String>),
}

impl NetworkAllow {
    /// Whether this admits traffic for `host`, the name the sandbox gives it. A list admits a name
    /// only `through_platform_dns`: where the platform's DNS gave the sandbox the address it dials,
    /// since otherwise the sandbox could name an allowed host for any address.
    #[must_use]
    pub fn admits(&self, host: &str, through_platform_dns: bool) -> bool {
        match self {
            Self::All => true,
            Self::Hosts(patterns) => {
                through_platform_dns
                    && patterns
                        .iter()
                        .any(|pattern| crate::host_pattern::matches(pattern, host))
            }
        }
    }
}

const NETWORK_ALLOW_ALL: &str = "all";

impl Serialize for NetworkAllow {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::All => serializer.serialize_str(NETWORK_ALLOW_ALL),
            Self::Hosts(patterns) => patterns.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for NetworkAllow {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = NetworkAllow;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("`all` or a list of host patterns")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<NetworkAllow, E> {
                if value == NETWORK_ALLOW_ALL {
                    Ok(NetworkAllow::All)
                } else {
                    Err(E::invalid_value(serde::de::Unexpected::Str(value), &self))
                }
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, sequence: A) -> Result<NetworkAllow, A::Error> {
                Vec::deserialize(serde::de::value::SeqAccessDeserializer::new(sequence)).map(NetworkAllow::Hosts)
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

/// Agent-layer network policy interpreted by the host Policy Engine.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NetworkSpec {
    /// Required mediation mode.
    pub mode: NetworkMode,
    /// Baseline egress decision.
    pub allow: NetworkAllow,
    /// Host patterns denied before the baseline decision.
    #[serde(default, deserialize_with = "null_as_default", skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
}

fn valid_environment_variable(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit()))
}

/// Most recently observed agent state.
///
/// Unlike the rest of the manifest, unknown fields are tolerated so an older
/// client can read responses from a newer control plane; status is
/// API-managed and never authored by hand.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Desired generation observed by the reconciler.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub observed_generation: u64,
    /// The restart, by its `metadata.restartTimestamp`, whose stop the reconciler has carried out.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub observed_restart_timestamp: Option<OffsetDateTime>,
    /// Sticky selected Provider and optional materialized sandbox identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<crate::sandbox::Assignment>,
    /// Normalized readiness conditions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// Classification of the reconciliation pass that recorded these
    /// conditions, when it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<crate::FailureKind>,
    /// Provisioning of the latest pass while it runs or after it failed.
    /// Projected onto API responses from the daemon's in-memory state; stores
    /// scrub it, so it is never persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<crate::progress::Provisioning>,
    /// Where the agent's manifest is. Projected onto API responses from the stored agent record;
    /// stores scrub it, so it is never persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// Where the agent's definition comes from. Projected onto API responses from the stored agent
    /// record; stores scrub it, so it is never persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// How long agentd keeps the agent, when it is not until someone deletes it. Projected like
    /// [`Self::origin`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifetime: Option<Lifetime>,
    /// The bind mount sources given when the agent was applied or run, by target, other than
    /// those its manifest names. Projected like [`Self::origin`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<SandboxPath, std::path::PathBuf>,
    /// The revision of each volume the agent mounts, once the reconciler has chosen them. Projected
    /// onto API responses from the stored agent record; stores scrub it, so it is never persisted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<MountedVolume>,
}

/// The revision of a volume an agent mounts, chosen when its sandbox is created.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MountedVolume {
    /// Where the volume is mounted.
    pub target: SandboxPath,
    /// The volume's name.
    pub volume: String,
    /// The revision mounted, or none for an optional volume left out until the agent is recreated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

impl Status {
    /// Creates reconciler-observed state; progress, provenance and volumes stay API-projected.
    #[must_use]
    pub const fn observed(
        observed_generation: u64,
        sandbox: Option<crate::sandbox::Assignment>,
        conditions: Vec<Condition>,
    ) -> Self {
        Self {
            observed_generation,
            observed_restart_timestamp: None,
            sandbox,
            conditions,
            failure: None,
            progress: None,
            provenance: None,
            origin: None,
            lifetime: None,
            bindings: BTreeMap::new(),
            volumes: Vec::new(),
        }
    }

    /// Whether agentd deletes the agent once the client that asked for it is gone. Such an agent
    /// is that client's to show, so listings leave it out.
    #[must_use]
    pub fn is_ephemeral(&self) -> bool {
        self.lifetime == Some(Lifetime::Ephemeral)
    }

    /// Removes what API responses project from the stored agent record and agentd's memory, which
    /// is never stored.
    pub(crate) fn scrub_projected(&mut self) {
        self.progress = None;
        self.provenance = None;
        self.origin = None;
        self.lifetime = None;
        self.bindings.clear();
        self.volumes.clear();
    }

    fn is_empty(&self) -> bool {
        self.observed_generation == 0
            && self.observed_restart_timestamp.is_none()
            && self.sandbox.is_none()
            && self.conditions.is_empty()
            && self.failure.is_none()
            && self.progress.is_none()
            && self.provenance.is_none()
            && self.origin.is_none()
            && self.lifetime.is_none()
            && self.bindings.is_empty()
            && self.volumes.is_empty()
    }

    /// Carries each condition's transition time forward from `previous`, the
    /// stored status being replaced, and stamps `now` on conditions whose
    /// status or reason changed.
    ///
    /// A message-only change is not a transition, so a retry that reports a
    /// different error detail keeps the time the condition entered its state.
    pub fn stamp_transitions(&mut self, previous: &Self, now: OffsetDateTime) {
        for condition in &mut self.conditions {
            let earlier = previous
                .conditions
                .iter()
                .find(|earlier| earlier.kind == condition.kind);
            condition.last_transition_time = match earlier {
                Some(earlier) if earlier.status == condition.status && earlier.reason == condition.reason => {
                    earlier.last_transition_time
                }
                _ => Some(now),
            };
        }
    }

    /// Returns the `Ready` condition when the reconciler has reported one.
    #[must_use]
    pub fn ready_condition(&self) -> Option<&Condition> {
        Condition::find_ready(&self.conditions)
    }

    /// Returns whether the reconciler reported `Ready=True`.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        Condition::any_ready(&self.conditions)
    }

    /// Returns whether a pass recorded the agent's sandbox as stopped.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.ready_condition()
            .is_some_and(|ready| ready.reason == Condition::REASON_STOPPED)
    }

    /// Returns the `SandboxResponsive=False` condition when the agent's guest
    /// is recorded as unresponsive.
    #[must_use]
    pub fn unresponsive(&self) -> Option<&Condition> {
        self.conditions.iter().find(|condition| {
            condition.kind == Condition::SANDBOX_RESPONSIVE && condition.status == ConditionStatus::False
        })
    }

    /// Returns the failure detail when desired state must change before
    /// another pass can succeed.
    #[must_use]
    pub fn invalid(&self) -> Option<String> {
        if self.failure != Some(crate::FailureKind::Invalid) {
            return None;
        }
        self.ready_condition()
            .or_else(|| self.conditions.first())
            .map(Condition::detail)
    }
}

impl Condition {
    /// Condition type summarizing whether the agent can serve sessions and Executions.
    pub const READY: &'static str = "Ready";
    /// Condition type for the sandbox lifecycle underneath `Ready`.
    pub const SANDBOX_READY: &'static str = "SandboxReady";
    /// Condition type for whether the running sandbox's guest still makes
    /// progress, observed by the host without a round trip to the guest.
    pub const SANDBOX_RESPONSIVE: &'static str = "SandboxResponsive";
    /// Condition type for whether the sandbox runs the nested sandboxes its agent asks for.
    pub const NESTED_SANDBOXES: &'static str = "NestedSandboxes";
    /// Condition type for whether the optional bind mounts, volumes, instructions and skills an agent declares exist.
    pub const OPTIONAL_SOURCES: &'static str = "OptionalSources";
    /// Condition type for an agent that still reads values from an environment file, which a later
    /// release stops reading. Its message names them and where to move them.
    pub const ENVIRONMENT_FILE: &'static str = "EnvironmentFile";
    /// Condition type for an agent the person agreed could read files that may hold real keys, such
    /// as a checkout's `.env`. Its message names them.
    pub const EXPOSED_FILES: &'static str = "ExposedFiles";
    /// Condition type for whether every account the agent asks for has a login kept, without which
    /// the agent runs without it. Its message names the command that keeps a missing one.
    pub const ACCOUNTS: &'static str = "Accounts";
    /// Condition type for whether the image cloned the repositories it lists into the workspace.
    pub const WORKSPACE: &'static str = "Workspace";
    /// Condition type for declared SSH access underneath `Ready`.
    pub const SSH_READY: &'static str = "SshReady";
    /// VNC access is reconciled and the bridge to the desktop is listening.
    pub const VNC_READY: &'static str = "VncReady";

    /// `Ready` reason while a pass stops the agent's sandbox, and after a stop failed.
    pub const REASON_STOPPING: &'static str = "Stopping";
    /// `Ready` and `SandboxReady` reason once the agent's sandbox is stopped.
    pub const REASON_STOPPED: &'static str = "Stopped";
    /// `Ready` reason while a pass starts a stopped sandbox.
    pub const REASON_STARTING: &'static str = "Starting";
    /// `Ready` reason while an agent waits for the volumes it mounts.
    pub const REASON_WAITING_FOR_VOLUMES: &'static str = "WaitingForVolumes";

    /// Finds the `Ready` condition in a condition list.
    #[must_use]
    pub fn find_ready(conditions: &[Self]) -> Option<&Self> {
        conditions.iter().find(|condition| condition.kind == Self::READY)
    }

    /// Returns whether a condition list reports `Ready=True`.
    #[must_use]
    pub fn any_ready(conditions: &[Self]) -> bool {
        Self::find_ready(conditions).is_some_and(|condition| condition.status == ConditionStatus::True)
    }

    /// The conditions saying that a ready agent works without something it needs, such as its
    /// GitHub account or its checkout. Readiness does not wait for them, so clients show them beside
    /// it, or the person finds out only once the agent fails at its work.
    pub fn shortfalls(conditions: &[Self]) -> impl Iterator<Item = &Self> {
        conditions.iter().filter(|condition| {
            [Self::ACCOUNTS, Self::WORKSPACE].contains(&condition.kind.as_str())
                && condition.status == ConditionStatus::False
        })
    }

    /// Returns `reason: message`, or whichever of the two is present, or `Unknown`.
    #[must_use]
    pub fn summary(&self) -> String {
        match (self.reason.is_empty(), self.message.is_empty()) {
            (false, false) => format!("{}: {}", self.reason, self.message),
            (false, true) => self.reason.clone(),
            (true, false) => self.message.clone(),
            (true, true) => "Unknown".to_owned(),
        }
    }

    /// Returns the human-readable message, falling back to the reason.
    #[must_use]
    pub fn detail(&self) -> String {
        if self.message.is_empty() {
            self.reason.clone()
        } else {
            self.message.clone()
        }
    }
}

/// Conventional agent manifest filename, used when a record predates path recording.
pub const MANIFEST_FILE: &str = "agent.yaml";

/// Where an agent's definition comes from, which agentd sets when it stores the agent.
///
/// An agent's definition can be updated but never replaced by another, so `apply` refuses a
/// manifest of another origin under the agent's name. Clients decide how to show an agent from
/// it, never from its name.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Origin {
    /// A manifest on this machine. [`Status::provenance`] says where it is.
    #[default]
    Local,
    /// A definition agentd resolved from a reference, as an image is named, which the agent reads
    /// from agentd's copy of it, as `status.provenance` says. See [`crate::DefinitionReference`].
    Reference {
        /// The reference the agent was run from, in its full form, such as `bundled/agent-builder`.
        reference: String,
    },
}

impl Origin {
    /// Returns whether the agent was applied from a manifest on this machine.
    #[must_use]
    pub const fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }
}

/// How long agentd keeps an agent it runs from its own definitions.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Lifetime {
    /// Until someone deletes it, as any applied agent.
    #[default]
    UntilDeleted,
    /// While the client that asked for it stays connected to agentd. agentd deletes it when that
    /// connection closes, and any left over from an earlier run of agentd when it starts.
    Ephemeral,
}

/// Local origin of an agent's desired state.
///
/// Part of [`Status`], so unknown fields are tolerated for the same reason.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    /// Absolute directory against which manifest-relative sources are resolved.
    pub source_directory: std::path::PathBuf,
    /// Absolute path of the manifest last applied, when the client reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<std::path::PathBuf>,
    /// Absolute path of the secret file, when it is not `.env` beside the manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_file: Option<std::path::PathBuf>,
}

impl Provenance {
    /// Returns the recorded manifest path, falling back to [`MANIFEST_FILE`]
    /// in the source directory for records that predate path recording.
    #[must_use]
    pub fn manifest_or_default(&self) -> std::path::PathBuf {
        self.manifest_path
            .clone()
            .unwrap_or_else(|| self.source_directory.join(MANIFEST_FILE))
    }
}

/// One aspect of observed agent state.
///
/// Like [`Status`], unknown fields are tolerated, so a client or store reader
/// can read conditions written by a newer control plane.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    /// Stable condition type.
    #[serde(rename = "type")]
    pub kind: String,
    /// Normalized truth value.
    pub status: ConditionStatus,
    /// Stable machine-readable reason.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// Optional human-readable detail.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    /// When `status` or `reason` last changed. Stamped by the store; absent on
    /// conditions recorded before transition times were tracked, until their
    /// next transition.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub last_transition_time: Option<OffsetDateTime>,
}

/// Truth value of an agent condition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ConditionStatus {
    /// The condition is satisfied.
    True,
    /// The condition is not satisfied.
    False,
    /// The control plane cannot determine the value.
    Unknown,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// Decodes and validates a YAML or JSON agent manifest.
///
/// # Errors
///
/// Returns an error when syntax, structure, or required values are invalid.
pub fn decode(bytes: &[u8]) -> Result<Agent, Error> {
    let agent: Agent = serde_yaml_ng::from_slice(bytes).map_err(|error| match other_version(&error) {
        Some(hint) => Error::Invalid(format!("{error}. {hint}")),
        None => Error::Yaml(error),
    })?;
    agent.validate()?;
    Ok(agent)
}

/// Expands a complete agent or chained agent variant from `path`.
///
/// Variant bases are restricted to conventional sibling filenames. A variant
/// applies to its base as a strategic merge patch does in Kustomize, with the
/// lists in [`KEYED_LISTS`] merged on their key. `agentctl/docs/variants.md`
/// has the rules and where they differ from Kustomize's.
///
/// # Errors
///
/// Returns an error when a manifest cannot be read, a variant chain is invalid,
/// or the expanded agent does not satisfy the strict agent contract.
pub fn resolve(path: &Path) -> Result<ResolvedManifest, Error> {
    let leaf = absolute_lexical(path)?;
    let mut chain = Vec::new();
    let mut versions = Vec::new();
    let value = resolve_value(&leaf, &mut chain, &mut versions).map_err(|error| with_chain(error, &chain))?;
    let bytes = serde_yaml_ng::to_string(&value).map_err(Error::Yaml)?;
    let agent = decode(bytes.as_bytes()).map_err(|error| manifest_error(&leaf, "expanded agent", &error))?;
    Ok(ResolvedManifest { agent, chain })
}

/// Extracts a variant selector from a conventional leaf filename.
#[must_use]
pub fn variant_from_filename(path: &Path) -> Option<AgentVariantName> {
    let name = path.file_name()?.to_str()?;
    let variant = name.strip_prefix("agent.")?.strip_suffix(".yaml")?;
    variant.parse().ok()
}

/// Returns whether a filename is a conventional agent or agent variant manifest.
#[must_use]
pub fn is_manifest_filename(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == MANIFEST_FILE) || variant_from_filename(path).is_some()
}

fn resolve_value(
    path: &Path,
    chain: &mut Vec<PathBuf>,
    versions: &mut Vec<(PathBuf, String)>,
) -> Result<serde_yaml_ng::Value, Error> {
    if let Some(start) = chain.iter().position(|candidate| candidate == path) {
        let cycle = chain[start..]
            .iter()
            .map(PathBuf::as_path)
            .chain(std::iter::once(path))
            .map(display_leaf)
            .collect::<Vec<_>>()
            .join(" -> ");
        return Err(Error::Invalid(format!("variant inheritance cycle: {cycle}")));
    }
    if chain.len() == MAX_VARIANT_CHAIN {
        return Err(Error::Invalid(format!(
            "{}: variant inheritance exceeds {MAX_VARIANT_CHAIN} manifests",
            display_leaf(path)
        )));
    }

    chain.push(path.to_path_buf());

    let bytes = std::fs::read(path)
        .map_err(|error| Error::Invalid(format!("{}: could not read manifest: {error}", display_leaf(path))))?;
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&bytes)
        .map_err(|error| Error::Invalid(format!("{}: {error}", display_leaf(path))))?;
    let mapping = value
        .as_mapping()
        .ok_or_else(|| Error::Invalid(format!("{}: manifest must be a mapping", display_leaf(path))))?;
    let version = string_field(mapping, "apiVersion", path)?;
    let kind = string_field(mapping, "kind", path)?;
    if let Some((version_path, expected)) = versions.first()
        && version != *expected
    {
        return Err(Error::Invalid(format!(
            "{}: apiVersion {version:?} does not match {} ({expected:?})",
            display_leaf(path),
            display_leaf(version_path)
        )));
    }
    versions.push((path.to_path_buf(), version));
    match kind.as_str() {
        KIND => {
            decode(&bytes).map_err(|error| manifest_error(path, "complete agent", &error))?;
            Ok(value)
        }
        VARIANT_KIND => {
            if variant_from_filename(path).is_none() {
                return Err(Error::Invalid(format!(
                    "{}: AgentVariant filename must match agent.<variant>.yaml",
                    display_leaf(path)
                )));
            }
            let variant: AgentVariant = serde_yaml_ng::from_slice(&bytes).map_err(|error| {
                let hint = other_version(&error)
                    .map(|hint| format!(". {hint}"))
                    .unwrap_or_default();
                Error::Invalid(format!("{}: {error}{hint}", display_leaf(path)))
            })?;
            validate_variant(&variant, path)?;
            let base = sibling_base(path, &variant.extends)?;
            let mut inherited = resolve_value(&base, chain, versions)?;
            let mut overlay = value;
            let overlay_mapping = overlay
                .as_mapping_mut()
                .ok_or_else(|| Error::Invalid(format!("{}: manifest must be a mapping", display_leaf(path))))?;
            overlay_mapping.remove(serde_yaml_ng::Value::String("extends".into()));
            overlay_mapping.insert(
                serde_yaml_ng::Value::String("kind".into()),
                serde_yaml_ng::Value::String(KIND.into()),
            );
            merge_value(&mut inherited, overlay, &mut Vec::new())
                .map_err(|message| Error::Invalid(format!("{}: {message}", display_leaf(path))))?;
            let expanded = serde_yaml_ng::to_string(&inherited).map_err(Error::Yaml)?;
            decode(expanded.as_bytes()).map_err(|error| manifest_error(path, "expanded variant", &error))?;
            Ok(inherited)
        }
        _ => Err(Error::Invalid(format!(
            "{}: kind must be {KIND:?} or {VARIANT_KIND:?}",
            display_leaf(path)
        ))),
    }
}

fn validate_variant(variant: &AgentVariant, path: &Path) -> Result<(), Error> {
    if variant.api_version != API_VERSION {
        return Err(Error::Invalid(format!(
            "{}: apiVersion must be {API_VERSION:?}",
            display_leaf(path)
        )));
    }
    if variant.kind != VARIANT_KIND {
        return Err(Error::Invalid(format!(
            "{}: kind must be {VARIANT_KIND:?}",
            display_leaf(path)
        )));
    }
    let metadata = variant
        .metadata
        .as_mapping()
        .ok_or_else(|| Error::Invalid(format!("{}: metadata must be a mapping", display_leaf(path))))?;
    let name = string_field(metadata, "name", path).map_err(|_| {
        Error::Invalid(format!(
            "{}: metadata.name must be explicitly specified",
            display_leaf(path)
        ))
    })?;
    if name.is_empty() {
        return Err(Error::Invalid(format!(
            "{}: metadata.name must be explicitly specified",
            display_leaf(path)
        )));
    }
    if let Some(spec) = &variant.spec
        && !spec.is_mapping()
    {
        return Err(Error::Invalid(format!(
            "{}: spec must be a mapping",
            display_leaf(path)
        )));
    }
    Ok(())
}

fn sibling_base(path: &Path, extends: &str) -> Result<PathBuf, Error> {
    let base = Path::new(extends);
    let one_normal_component = {
        let mut components = base.components();
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
    };
    if !one_normal_component || !is_manifest_filename(base) {
        return Err(Error::Invalid(format!(
            "{}: extends must name agent.yaml or a sibling agent.<variant>.yaml",
            display_leaf(path)
        )));
    }
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid(format!("{}: manifest path has no parent directory", display_leaf(path))))?;
    Ok(parent.join(base))
}

/// The field that carries a strategic merge patch directive in a variant.
const PATCH_FIELD: &str = "$patch";

/// The strategic merge patch directives that Kustomize and Kubernetes both take for the same purpose. A variant refuses
/// the others rather than pick one tool's reading.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Directive {
    /// Removes the inherited mapping, or the inherited list entry with the same key.
    Delete,
    /// Replaces the inherited mapping, or, as a list entry of its own, the inherited list.
    Replace,
}

impl Directive {
    /// Removes the directive from `value` when it is a mapping. `fields` lead to `value`.
    fn take(value: &mut serde_yaml_ng::Value, fields: &[String]) -> Result<Option<Self>, String> {
        let Some(directive) = value
            .as_mapping_mut()
            .and_then(|mapping| mapping.remove(serde_yaml_ng::Value::String(PATCH_FIELD.into())))
        else {
            return Ok(None);
        };
        match directive.as_str() {
            Some("delete") => Ok(Some(Self::Delete)),
            Some("replace") => Ok(Some(Self::Replace)),
            _ => Err(format!(
                "{}: {PATCH_FIELD} must be \"delete\" or \"replace\"",
                fields.join(".")
            )),
        }
    }
}

/// The lists a variant merges entry by entry, by the field names that lead to them, with each entry's key. The key is
/// what validation already requires to be unique in the list, so an entry in a variant names one inherited entry.
/// Other lists replace the inherited one whole, since a list of strings has no key.
const KEYED_LISTS: &[(&[&str], ListKey)] = &[
    (&["spec", "access"], ListKey::Field("type")),
    (&["spec", "accounts"], ListKey::Field("type")),
    (&["spec", "harnesses"], ListKey::Field("type")),
    (&["spec", "environment"], ListKey::Field("name")),
    (&["spec", "secrets"], ListKey::Field("environment")),
    (&["spec", "instructions"], ListKey::Field("source")),
    (&["spec", "skills"], ListKey::Skill),
    (&["spec", "sandbox", "mounts"], ListKey::Field("target")),
];

/// How an entry of a keyed list names itself.
#[derive(Clone, Copy)]
enum ListKey {
    /// A string field.
    Field(&'static str),
    /// A skill's `name`, else its source directory's name, as [`SkillSpec::name`] reads it.
    Skill,
}

impl ListKey {
    fn of(self, entry: &serde_yaml_ng::Value) -> Option<String> {
        let mapping = entry.as_mapping()?;
        let field = |name: &str| {
            mapping
                .get(serde_yaml_ng::Value::String(name.into()))
                .and_then(serde_yaml_ng::Value::as_str)
        };
        let key = match self {
            Self::Field(name) => field(name),
            Self::Skill => field("name").or_else(|| Path::new(field("source")?).file_name()?.to_str()),
        };
        key.map(str::to_owned)
    }

    const fn field(self) -> &'static str {
        match self {
            Self::Field(name) => name,
            Self::Skill => "name or source",
        }
    }

    fn at(fields: &[String]) -> Option<Self> {
        KEYED_LISTS
            .iter()
            .find(|(list, _)| list.iter().copied().eq(fields.iter().map(String::as_str)))
            .map(|(_, key)| *key)
    }
}

/// Merges a variant's `patch` into the inherited `base`. `fields` holds the field names that lead to `base`, so a keyed
/// list is recognised where it sits. Errors name the field but not the file, which the caller adds.
fn merge_value(
    base: &mut serde_yaml_ng::Value,
    patch: serde_yaml_ng::Value,
    fields: &mut Vec<String>,
) -> Result<(), String> {
    if let (Some(base_mapping), Some(patch_mapping)) = (base.as_mapping(), patch.as_mapping()) {
        let discriminator = serde_yaml_ng::Value::String("type".into());
        if let (Some(base_type), Some(patch_type)) = (
            base_mapping.get(&discriminator).and_then(serde_yaml_ng::Value::as_str),
            patch_mapping.get(&discriminator).and_then(serde_yaml_ng::Value::as_str),
        ) && base_type != patch_type
        {
            // Internally tagged union values are one logical scalar choice.
            // Changing the discriminator replaces the whole mapping so fields
            // belonging to the previous variant cannot leak into the new one.
            *base = serde_yaml_ng::Value::Mapping(serde_yaml_ng::Mapping::new());
        }
    }
    match (base, patch) {
        (serde_yaml_ng::Value::Mapping(base), serde_yaml_ng::Value::Mapping(patch)) => {
            for (key, value) in patch {
                fields.push(key.as_str().unwrap_or_default().to_owned());
                let merged = merge_field(base, key, value, fields);
                fields.pop();
                merged?;
            }
            Ok(())
        }
        (serde_yaml_ng::Value::Sequence(base), serde_yaml_ng::Value::Sequence(patch)) => {
            if let Some(key) = ListKey::at(fields) {
                merge_list(base, patch, key, fields)
            } else {
                *base = patch;
                Ok(())
            }
        }
        (base, patch) => {
            *base = patch;
            Ok(())
        }
    }
}

fn merge_field(
    base: &mut serde_yaml_ng::Mapping,
    key: serde_yaml_ng::Value,
    mut value: serde_yaml_ng::Value,
    fields: &mut Vec<String>,
) -> Result<(), String> {
    let directive = Directive::take(&mut value, fields)?;
    if directive == Some(Directive::Delete) {
        value = serde_yaml_ng::Value::Null;
    }
    if value.is_null() {
        if base.remove(&key).is_none() {
            // Preserve a null for fields absent from the concrete base.
            // Known fields decode it as their default, while strict
            // deserialization rejects unknown null-valued fields.
            base.insert(key, value);
        }
        return Ok(());
    }
    match base.get_mut(&key) {
        Some(inherited) if directive.is_none() => merge_value(inherited, value, fields),
        _ => {
            // A new or replacing value is merged into an empty one, which applies the directives inside it.
            let mut fresh = match value {
                serde_yaml_ng::Value::Mapping(_) => serde_yaml_ng::Value::Mapping(serde_yaml_ng::Mapping::new()),
                serde_yaml_ng::Value::Sequence(_) => serde_yaml_ng::Value::Sequence(Vec::new()),
                _ => serde_yaml_ng::Value::Null,
            };
            merge_value(&mut fresh, value, fields)?;
            base.insert(key, fresh);
            Ok(())
        }
    }
}

/// Merges each entry of a variant's keyed list into the inherited entry with its key, keeping the inherited order and
/// adding new keys after it. An entry marked `$patch: delete` removes the inherited one, and an entry holding only
/// `$patch: replace` makes the variant's entries replace the inherited list. Removing a key the base does not have is
/// not an error, so a variant keeps working after its base drops the entry.
fn merge_list(
    base: &mut Vec<serde_yaml_ng::Value>,
    patch: Vec<serde_yaml_ng::Value>,
    key: ListKey,
    fields: &mut Vec<String>,
) -> Result<(), String> {
    let field = fields.join(".");
    if patch.is_empty() {
        // Kustomize leaves the inherited entries as they are, which is rarely what an empty list means.
        return Err(format!(
            "{field}: an empty list changes nothing in a variant. Write null to remove the inherited entries"
        ));
    }
    let mut entries = Vec::with_capacity(patch.len());
    let mut names = std::collections::BTreeSet::new();
    let mut replace = false;
    for mut entry in patch {
        let directive = Directive::take(&mut entry, fields)?;
        let name = key.of(&entry);
        match (directive, &name) {
            (Some(Directive::Replace), None) if entry.as_mapping().is_some_and(serde_yaml_ng::Mapping::is_empty) => {
                replace = true;
                continue;
            }
            (Some(Directive::Replace), _) => {
                return Err(format!(
                    "{field}: {PATCH_FIELD}: replace goes in an entry of its own, where it replaces the inherited list"
                ));
            }
            (Some(Directive::Delete), None) => {
                return Err(format!(
                    "{field}: an entry with {PATCH_FIELD}: delete must name the entry it removes by its {}",
                    key.field()
                ));
            }
            _ => {}
        }
        if let Some(name) = &name
            && !names.insert(name.clone())
        {
            return Err(format!("{field}: two entries have the {} {name:?}", key.field()));
        }
        entries.push((directive == Some(Directive::Delete), name, entry));
    }
    if replace {
        base.clear();
    }
    for (delete, name, entry) in entries {
        let inherited = name.and_then(|name| {
            base.iter()
                .position(|candidate| key.of(candidate).as_deref() == Some(&name))
        });
        match (delete, inherited) {
            (true, Some(index)) => {
                base.remove(index);
            }
            (true, None) => {}
            (false, Some(index)) => merge_value(&mut base[index], entry, fields)?,
            (false, None) => {
                // Merging into an empty entry applies the directives inside it. An entry without its key is added too,
                // for decoding to report what it lacks.
                let mut added = serde_yaml_ng::Value::Mapping(serde_yaml_ng::Mapping::new());
                merge_value(&mut added, entry, fields)?;
                base.push(added);
            }
        }
    }
    Ok(())
}

fn string_field(mapping: &serde_yaml_ng::Mapping, field: &str, path: &Path) -> Result<String, Error> {
    mapping
        .get(serde_yaml_ng::Value::String(field.into()))
        .and_then(serde_yaml_ng::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::Invalid(format!("{}: {field} must be a string", display_leaf(path))))
}

/// Resolves a manifest host path: `~` and `~/…` below the home directory, a relative path against the manifest's
/// directory.
pub(crate) fn resolve_host_path(source_directory: &Path, path: &Path, field: &str) -> Result<PathBuf, Error> {
    resolve_host_path_with(source_directory, path, field, crate::local::home::user_home_directory)
}

/// Resolves a manifest host path as [`resolve_host_path`] does, or `None` when the source is optional and the path
/// does not exist, which includes a symbolic link to nothing. Only a path that does not exist is forgiven: one that
/// cannot be checked still fails, naming `field` and the path.
pub(crate) fn resolve_present_host_path(
    source_directory: &Path,
    path: &Path,
    optional: bool,
    field: &str,
) -> Result<Option<PathBuf>, Error> {
    let path = resolve_host_path(source_directory, path, field)?;
    let present = !optional
        || path
            .try_exists()
            .map_err(|error| std::io::Error::new(error.kind(), format!("{field} {}: {error}", path.display())))?;
    Ok(present.then_some(path))
}

fn resolve_host_path_with(
    source_directory: &Path,
    path: &Path,
    field: &str,
    home: impl FnOnce() -> Option<PathBuf>,
) -> Result<PathBuf, Error> {
    let mut components = path.components();
    if components.next() == Some(Component::Normal("~".as_ref())) {
        let home = home()
            .ok_or_else(|| Error::Invalid(format!("{field} starts with ~, but the home directory is not set")))?;
        let rest = components.as_path();
        return Ok(if rest.as_os_str().is_empty() {
            home
        } else {
            home.join(rest)
        });
    }
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        source_directory.join(path)
    })
}

fn absolute_lexical(path: &Path) -> Result<PathBuf, Error> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn display_leaf(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// What to do about a manifest `error` that names a field or a value this version does not know,
/// which the manifest's own version of agentctl would: update agentctl for a manifest written for
/// a later version, or the manifest for one written for an earlier version.
fn other_version(error: &serde_yaml_ng::Error) -> Option<String> {
    let message = error.to_string();
    (message.contains("unknown field") || message.contains("unknown variant")).then(|| {
        format!(
            "This is agentctl {}. A manifest written for a later version needs `agentctl self update`, and \
             `agentctl update` brings one written for an earlier version up to date",
            crate::build_version()
        )
    })
}

/// An error in the manifest at `path`, named by its file. A validation message is shown as it is;
/// any other error says which form of the manifest, `context`, could not be read.
fn manifest_error(path: &Path, context: &str, error: &Error) -> Error {
    match error {
        Error::Invalid(message) => Error::Invalid(format!("{}: {message}", display_leaf(path))),
        error => Error::Invalid(format!("{}: invalid {context}: {error}", display_leaf(path))),
    }
}

fn with_chain(error: Error, chain: &[PathBuf]) -> Error {
    if chain.len() < 2 || error.to_string().contains("inheritance chain:") {
        return error;
    }
    let mut lines = chain.iter().map(|path| display_leaf(path));
    let Some(first) = lines.next() else {
        return error;
    };
    let diagnostic = std::iter::once(first)
        .chain(lines.map(|line| format!("  extends {line}")))
        .collect::<Vec<_>>()
        .join("\n");
    Error::Invalid(format!("{error}\ninheritance chain:\n{diagnostic}"))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::resolve_host_path_with;

    fn resolve(path: &str, home: Option<&str>) -> Result<PathBuf, crate::Error> {
        resolve_host_path_with(Path::new("/manifests/worker"), Path::new(path), "spec.field", || {
            home.map(PathBuf::from)
        })
    }

    #[test]
    fn a_leading_tilde_names_the_home_directory_and_other_paths_are_unchanged() {
        let resolved = |path| resolve(path, Some("/users/person")).ok();
        assert_eq!(
            resolved("~/skills/review"),
            Some(PathBuf::from("/users/person/skills/review"))
        );
        assert_eq!(resolved("~/"), Some(PathBuf::from("/users/person")));
        assert_eq!(resolved("~"), Some(PathBuf::from("/users/person")));
        assert_eq!(
            resolved("skills/review"),
            Some(PathBuf::from("/manifests/worker/skills/review"))
        );
        assert_eq!(
            resolved("../shared"),
            Some(PathBuf::from("/manifests/worker/../shared"))
        );
        assert_eq!(
            resolved("~other/skills"),
            Some(PathBuf::from("/manifests/worker/~other/skills"))
        );
        assert_eq!(resolved("skills/~"), Some(PathBuf::from("/manifests/worker/skills/~")));
        let absolute = std::env::temp_dir().join("skills");
        assert_eq!(
            resolve(absolute.to_str().unwrap_or_default(), Some("/users/person")).ok(),
            Some(absolute)
        );
    }

    #[test]
    fn only_an_optional_path_that_does_not_exist_is_left_out() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        std::fs::write(directory.path().join("present.md"), "present").expect("present file");
        let present = |path: &str, optional| {
            super::resolve_present_host_path(directory.path(), Path::new(path), optional, "spec.field")
        };

        for (path, optional, expected) in [
            ("present.md", true, true),
            ("present.md", false, true),
            ("absent.md", false, true),
            ("absent.md", true, false),
        ] {
            let resolved = present(path, optional).expect("checked");
            assert_eq!(resolved.is_some(), expected, "{path} optional={optional}");
        }

        // Windows reports a path below a file as missing, and needs privileges for symbolic links.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(directory.path().join("absent.md"), directory.path().join("dangling.md"))
                .expect("dangling link");
            assert_eq!(present("dangling.md", true).expect("checked"), None);
            let error = present("present.md/inner.md", true).expect_err("a path below a file cannot be checked");
            assert!(
                error.to_string().contains("spec.field ") && error.to_string().contains("present.md/inner.md"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_leading_tilde_without_a_home_directory_is_rejected() {
        assert!(matches!(
            resolve("~/skills", None),
            Err(crate::Error::Invalid(message)) if message == "spec.field starts with ~, but the home directory is not set"
        ));
        assert!(resolve("skills", None).is_ok());
    }
}
