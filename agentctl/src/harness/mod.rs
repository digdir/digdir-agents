//! Harness-specific adapters behind the closed agent manifest harness selection.

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{Error, persistence};

mod claude_code;
mod codex;
mod hook_script;
mod login;
mod pi;
mod skills;

const VERSION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const MAX_INITIAL_PROMPT_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_SELECTION_CHARACTERS: usize = 128;

pub use login::LoginName;
pub(crate) use skills::{Skill, SkillFile, platform_skill, remove_linux as remove_skills_linux};

/// Supported harnesses.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Harness {
    /// Anthropic Claude Code.
    ClaudeCode,
    /// `OpenAI` Codex CLI.
    Codex,
    /// Pi, configured entirely by the agent: its providers, models and credentials.
    Pi,
}

impl Harness {
    /// The only authentication mode the harness accepts in a manifest.
    #[must_use]
    pub const fn auth_mode(self) -> HarnessAuthMode {
        match self {
            Self::ClaudeCode | Self::Codex => HarnessAuthMode::Mediated,
            Self::Pi => HarnessAuthMode::None,
        }
    }

    /// Returns the manifest and CLI spelling of this harness family.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claudeCode",
            Self::Codex => "codex",
            Self::Pi => "pi",
        }
    }
}

impl Harness {
    /// The harness's product name, as people know it.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Pi => "Pi",
        }
    }
}

impl std::fmt::Display for Harness {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for Harness {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "claudeCode" => Ok(Self::ClaudeCode),
            "codex" => Ok(Self::Codex),
            "pi" => Ok(Self::Pi),
            _ => Err(Error::Invalid(format!("unsupported harness {value:?}"))),
        }
    }
}

/// Supported harness authentication modes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HarnessAuthMode {
    /// Credentials remain on the host and are injected into authorized requests.
    Mediated,
    /// The harness has no platform-managed login; the agent supplies its credentials, for
    /// example through `spec.secrets` and the harness's own configuration files.
    None,
}

impl HarnessAuthMode {
    /// Returns the manifest spelling of this mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mediated => "mediated",
            Self::None => "none",
        }
    }
}

/// Declares a validated, provider-owned launch selection carried as an opaque string.
///
/// Model names and effort levels belong to the harness vendor: they differ between
/// harnesses and gain new values without a platform release, so the platform only
/// checks that a value can travel safely to the harness command line.
macro_rules! launch_selection {
    ($(#[$doc:meta])* $name:ident, $label:literal) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Creates a validated selection.
            ///
            /// # Errors
            ///
            /// Returns an error unless the value is 1–128 ASCII letters, digits or
            /// `-`, `_`, `.`, `:`, `/`, `@`, `+`.
            pub fn new(value: impl Into<String>) -> Result<Self, Error> {
                let value = value.into();
                validate_selection($label, &value)?;
                Ok(Self(value))
            }

            /// Returns the selection as the text handed to the harness.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = Error;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl std::str::FromStr for $name {
            type Err = Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

launch_selection! {
    /// Harness-owned model selection, such as a Claude Code alias or a Codex model name.
    Model, "model"
}

launch_selection! {
    /// Harness-owned effort or reasoning level, such as `high`.
    Effort, "effort"
}

/// A harness's model and effort level, each optional and provider-owned.
///
/// Declared on a harness installation as the defaults for its new sessions,
/// requested when a session is created, and recorded with the session as the
/// selection every launch of its harness applies.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelSelection {
    /// Model name in the harness's own spelling; `None` leaves the harness default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Model>,
    /// Effort level in the harness's own spelling; `None` leaves the harness default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
}

impl ModelSelection {
    /// Whether neither a model nor an effort level is selected.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.model.is_none() && self.effort.is_none()
    }

    /// Fills each unselected field from `defaults`, field by field.
    #[must_use]
    pub fn or(self, defaults: &Self) -> Self {
        Self {
            model: self.model.or_else(|| defaults.model.clone()),
            effort: self.effort.or_else(|| defaults.effort.clone()),
        }
    }

    /// Returns the model as text, when selected.
    #[must_use]
    pub fn model_str(&self) -> Option<&str> {
        self.model.as_ref().map(Model::as_str)
    }

    /// Returns the effort level as text, when selected.
    #[must_use]
    pub fn effort_str(&self) -> Option<&str> {
        self.effort.as_ref().map(Effort::as_str)
    }

    /// Describes the first field `requested` selects differently from this
    /// recorded selection, as a clause following the session's name; `None`
    /// when every requested field matches or is unselected.
    #[must_use]
    pub fn conflict_with(&self, requested: &Self) -> Option<String> {
        [
            ("model", self.model_str(), requested.model_str()),
            ("effort", self.effort_str(), requested.effort_str()),
        ]
        .into_iter()
        .find_map(|(field, recorded, requested)| {
            let requested = requested.filter(|requested| Some(*requested) != recorded)?;
            Some(recorded.map_or_else(
                || format!("leaves the {field} to the harness default, not {requested:?}"),
                |recorded| format!("already uses {field} {recorded:?}, not {requested:?}"),
            ))
        })
    }
}

fn validate_selection(label: &str, value: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.chars().count() > MAX_SELECTION_CHARACTERS
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/' | b'@' | b'+'))
    {
        return Err(Error::Invalid(format!(
            "{label} must be 1-{MAX_SELECTION_CHARACTERS} ASCII letters, digits or '-', '_', '.', ':', '/', '@', '+'"
        )));
    }
    Ok(())
}

/// One harness installation declared for an agent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(from = "DeclaredHarnessSpec", rename_all = "camelCase")]
pub struct HarnessSpec {
    /// Closed harness family identifier.
    #[serde(rename = "type")]
    pub kind: Harness,
    /// Exact version installed by the agent image; omitted when the image owns the version, so image bumps need no manifest change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The host login whose credential this installation's requests carry. Changing it re-binds
    /// the agent's mediation in place.
    #[serde(default, skip_serializing_if = "LoginName::is_default")]
    pub login: LoginName,
    /// Authentication delivery mode. A manifest may omit it, because each harness accepts exactly
    /// one ([`Harness::auth_mode`]); it is always stored.
    pub auth: HarnessAuthMode,
    /// Whether a host login this harness cannot find omits the installation instead of blocking
    /// agent provisioning. Re-evaluated on every pass, so a later host login installs it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    /// Whether new sessions select this installation when no harness is specified.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub default: bool,
    /// Model and effort level for new sessions of this installation that do not
    /// select their own. Omitted, the harness picks its own defaults.
    #[serde(default, skip_serializing_if = "ModelSelection::is_empty")]
    pub defaults: ModelSelection,
}

/// A harness installation as a manifest spells it, where `auth` may be omitted.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DeclaredHarnessSpec {
    #[serde(rename = "type")]
    kind: Harness,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    login: LoginName,
    #[serde(default)]
    auth: Option<HarnessAuthMode>,
    #[serde(default)]
    optional: bool,
    #[serde(default)]
    default: bool,
    #[serde(default)]
    defaults: ModelSelection,
}

impl From<DeclaredHarnessSpec> for HarnessSpec {
    fn from(declared: DeclaredHarnessSpec) -> Self {
        Self {
            auth: declared.auth.unwrap_or_else(|| declared.kind.auth_mode()),
            kind: declared.kind,
            version: declared.version,
            login: declared.login,
            optional: declared.optional,
            default: declared.default,
            defaults: declared.defaults,
        }
    }
}

/// Non-secret result of importing a host harness login.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImportedAuthentication {
    /// Authentication provider identifier.
    pub provider: String,
    /// The name the login is stored under.
    pub login: LoginName,
    /// Whether usable credentials were imported.
    pub ready: bool,
}

/// A stored harness login, without its credential.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StoredLogin {
    /// The harness the login is for.
    pub harness: Harness,
    /// The name it is stored under.
    pub login: LoginName,
}

/// Dispatches host-side authentication to the selected harness adapter.
pub struct AuthenticationManager {
    database: persistence::Database,
    claude_code: claude_code::authentication::Authentication,
    codex: codex::authentication::Authentication,
}

impl AuthenticationManager {
    /// Creates the harness authentication manager over the shared database owner.
    #[must_use]
    pub fn new(database: persistence::Database) -> Self {
        Self {
            database: database.clone(),
            claude_code: claude_code::authentication::Authentication::new(database.clone()),
            codex: codex::authentication::Authentication::new(database),
        }
    }

    /// Stores a credential for the selected harness as the login `login`, replacing an earlier
    /// login of that name.
    ///
    /// `imported` marks a credential supplied by the caller rather than minted by the host login
    /// flow; adapters whose host grant must stay isolated only accept mediated placeholders that way.
    ///
    /// # Errors
    ///
    /// Returns an error when the credential is invalid or cannot be persisted.
    pub async fn login(
        &self,
        harness: Harness,
        login: LoginName,
        credential: Zeroizing<String>,
        imported: bool,
    ) -> Result<ImportedAuthentication, Error> {
        match harness {
            Harness::ClaudeCode => self.claude_code.login(login, credential).await,
            Harness::Codex => self.codex.login(login, credential, imported).await,
            Harness::Pi => Err(no_platform_login()),
        }
    }

    /// Forgets the selected harness's login `login`, returning whether it was stored.
    ///
    /// # Errors
    ///
    /// Returns an error when the harness has no platform login or the database cannot be written.
    pub async fn logout(&self, harness: Harness, login: &LoginName) -> Result<bool, Error> {
        match harness {
            Harness::ClaudeCode => claude_code::LOGINS.delete(&self.database, login).await,
            Harness::Codex => self.codex.logout(login).await,
            Harness::Pi => Err(no_platform_login()),
        }
    }

    /// The stored logins of every harness, without their credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be read.
    pub async fn logins(&self) -> Result<Vec<StoredLogin>, Error> {
        let mut logins = Vec::new();
        for (harness, store) in [
            (Harness::ClaudeCode, &claude_code::LOGINS),
            (Harness::Codex, &codex::LOGINS),
        ] {
            for login in store.logins(&self.database).await? {
                logins.push(StoredLogin { harness, login });
            }
        }
        Ok(logins)
    }
}

impl sandbox::secret_store::SecretStore for AuthenticationManager {
    fn set<'a>(
        &'a self,
        name: &'a str,
        value: &'a [u8],
    ) -> sandbox::LocalFuture<'a, Result<sandbox::secret_store::SecretReference, sandbox::Error>> {
        sandbox::secret_store::SecretStore::set(&self.database, name, value)
    }

    fn resolve<'a>(
        &'a self,
        reference: &'a sandbox::secret_store::SecretReference,
    ) -> sandbox::LocalFuture<'a, Result<sandbox::secret_store::SecretMaterial, sandbox::Error>> {
        Box::pin(async move {
            if let Some(login) = codex::access_login(reference) {
                self.codex.resolve_access(&login).await
            } else {
                sandbox::secret_store::SecretStore::resolve(&self.database, reference).await
            }
        })
    }
}

/// Where an interactive harness login sends the person to sign in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SignIn {
    /// A browser on this host, opened by the harness's own opener or, when
    /// given, by this one, as under WSL where Linux openers cannot reach the
    /// Windows desktop.
    Browser(Option<UrlOpener>),
    /// Any browser elsewhere: none opened here would reach the person, for
    /// `reason`, so the harness shows a link to open on another device.
    Elsewhere { reason: String },
}

/// A program that opens the address given as its only argument when run with
/// the `environment` variable, a name and value, set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UrlOpener {
    pub program: std::path::PathBuf,
    pub environment: (&'static str, &'static str),
}

/// Acquires a host credential for the selected harness, interactively.
///
/// Runs on the client host, where a terminal is available; the
/// harness-specific login mechanism lives behind the closed harness enum.
/// `sign_in` is decided only for a harness whose login follows it; Codex's
/// shows its sign-in link itself.
///
/// # Errors
///
/// Returns an error when the harness login tool is missing, fails, or yields no credential.
pub fn acquire_host_credential(
    harness: Harness,
    control_plane_home: &std::path::Path,
    sign_in: impl FnOnce() -> SignIn,
) -> Result<Zeroizing<String>, Error> {
    match harness {
        Harness::ClaudeCode => claude_code::acquire_host_token(sign_in()),
        Harness::Codex => codex::acquire_host_credential(control_plane_home),
        Harness::Pi => Err(no_platform_login()),
    }
}

fn no_platform_login() -> Error {
    Error::Invalid("Pi has no platform login; the agent supplies its credentials".into())
}

/// The command that stores `login` for `harness`, for messages that ask for it.
#[must_use]
pub fn login_command(harness: Harness, login: &LoginName) -> String {
    let command = match harness {
        Harness::ClaudeCode => "claude",
        Harness::Codex => "codex",
        // Pi has no platform login, so nothing asks for one.
        Harness::Pi => return String::new(),
    };
    if login.is_default() {
        format!("`agentctl {command} login`")
    } else {
        format!("`agentctl {command} login {login}`")
    }
}

/// The error for an installation whose login `login` is not stored.
pub(crate) fn login_missing(harness: Harness, login: &LoginName) -> Error {
    Error::Invalid(format!(
        "{} login \"{login}\" is not stored; run {}",
        harness.display_name(),
        login_command(harness, login)
    ))
}

/// Whether this process runs inside an agent's sandbox: in a session, an Execution or an SSH login.
///
/// Agent setup writes [`crate::sandbox::platform::PLATFORM_API_URL_FILE`] into every agent's
/// sandbox and nowhere else, so its presence is the signal; see that constant.
#[must_use]
pub fn inside_agent() -> bool {
    std::path::Path::new(crate::sandbox::platform::PLATFORM_API_URL_FILE).exists()
}

/// The credential for the harness that an agent holds, for a login run `inside` it.
///
/// It is the mediated placeholder the agent's sessions carry, never a real credential. Importing it
/// lets a nested `agentd` chain through the outer agent's mediation, so inside an agent a login
/// needs no sign-in. `None` outside an agent, where a login signs in.
///
/// # Errors
///
/// Returns an error inside an agent without the harness, where no sign-in could complete, or when
/// the agent's credential cannot be read.
pub fn agent_credential(
    harness: Harness,
    inside: bool,
    environment: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Result<Option<Zeroizing<String>>, Error> {
    if !inside {
        return Ok(None);
    }
    match harness {
        Harness::ClaudeCode => claude_code::agent_credential(&environment).map(Some),
        Harness::Codex => codex::agent_credential(&environment).map(Some),
        Harness::Pi => Ok(None),
    }
}

/// Refuses a login inside an agent that has no login of `harness` to reuse, for `reason`.
fn no_agent_login(harness: Harness, reason: &str) -> Error {
    let name = harness.display_name();
    Error::Invalid(format!(
        "this agent has no {name} login to reuse: {reason}. Its manifest must declare {name} and the host be logged \
         in to it, which restarts this agent; or pass a credential with `--from-stdin`"
    ))
}

/// Reports whether the host login this harness mediates is present and usable.
///
/// An optional installation is omitted rather than failing provisioning when this is false, so the
/// check must distinguish "no login here" from a genuine fault, which stays an error.
pub(crate) async fn authentication_ready(
    installation: &HarnessSpec,
    database: &persistence::Database,
) -> Result<bool, Error> {
    match installation.kind {
        Harness::ClaudeCode => claude_code::authentication_ready(database, &installation.login).await,
        Harness::Codex => codex::authentication_ready(database, &installation.login).await,
        // No login to wait for: the agent supplies Pi's credentials itself.
        Harness::Pi => Ok(true),
    }
}

pub(crate) async fn prepare(
    installation: &HarnessSpec,
    database: &persistence::Database,
) -> Result<Vec<MediatedSecret>, Error> {
    match installation.kind {
        Harness::ClaudeCode => claude_code::prepare(database, &installation.login).await,
        Harness::Codex => codex::prepare(database, &installation.login).await,
        Harness::Pi => Ok(Vec::new()),
    }
}

pub(crate) struct MediatedSecret {
    pub(crate) environment: &'static str,
    pub(crate) placeholder: String,
    pub(crate) reference: sandbox::secret_store::SecretReference,
    pub(crate) allowed_hosts: Vec<String>,
}

pub(crate) fn conflicts_with_managed_secret(harness: Harness, name: &str, placeholder: Option<&str>) -> bool {
    match harness {
        Harness::ClaudeCode => claude_code::conflicts_with_managed_secret(name, placeholder),
        Harness::Codex => codex::conflicts_with_managed_secret(name, placeholder),
        Harness::Pi => false,
    }
}

pub(crate) fn manages_environment(harness: Harness, name: &str) -> bool {
    match harness {
        Harness::ClaudeCode => claude_code::manages_environment(name),
        Harness::Codex => codex::manages_environment(name),
        Harness::Pi => false,
    }
}

pub(crate) async fn bootstrap_linux(
    harness: Harness,
    sandbox: &sandbox::SandboxHandle,
    home: &str,
    instructions: &[u8],
    skills: &[Skill],
) -> Result<(), Error> {
    match harness {
        Harness::ClaudeCode => claude_code::bootstrap_linux(sandbox, home, instructions, skills).await,
        Harness::Codex => codex::bootstrap_linux(sandbox, home, instructions, skills).await,
        Harness::Pi => pi::bootstrap_linux(sandbox, home, instructions, skills).await,
    }
}

/// Where the harness reads the agent's instructions from.
pub(crate) fn instructions_path(harness: Harness, home: &str) -> String {
    match harness {
        Harness::ClaudeCode => claude_code::instructions_path(home),
        Harness::Codex => codex::instructions_path(home),
        Harness::Pi => pi::instructions_path(home),
    }
}

/// Where `harness` finds its skills, one folder each.
pub(crate) fn skills_directory(harness: Harness, home: &str) -> String {
    match harness {
        Harness::ClaudeCode => claude_code::skills_directory(home),
        Harness::Codex => codex::skills_directory(home),
        Harness::Pi => pi::skills_directory(home),
    }
}

/// Verifies that the declared harness installation exists, at the exact version when one is declared.
pub(crate) async fn verify_linux(
    harness: Harness,
    sandbox: &sandbox::SandboxHandle,
    expected_version: Option<&str>,
) -> Result<(), Error> {
    match harness {
        Harness::ClaudeCode => claude_code::verify_linux(sandbox, expected_version).await,
        Harness::Codex => codex::verify_linux(sandbox, expected_version).await,
        Harness::Pi => pi::verify_linux(sandbox, expected_version).await,
    }
}

async fn version_output(
    sandbox: &sandbox::SandboxHandle,
    executable: &str,
) -> Result<sandbox::execution::ExecutionOutput, Error> {
    use sandbox::{SandboxPath, execution::ExecutionSpec};

    let started = sandbox
        .start_execution(sandbox::execution::StartExecutionRequest::new(ExecutionSpec::command(
            SandboxPath::new("/usr/bin/env"),
            [executable.to_owned(), "--version".into()],
        )))
        .await?;
    let execution_id = started.id.clone();
    match tokio::time::timeout(VERSION_PROBE_TIMEOUT, started.collect()).await {
        Ok(output) => output.map_err(Error::from),
        Err(_elapsed) => {
            let _ignored = sandbox.kill_execution(&execution_id).await;
            Err(Error::SandboxSetup(format!(
                "`{executable} --version` did not finish within {}s",
                VERSION_PROBE_TIMEOUT.as_secs()
            )))
        }
    }
}

/// Harness-specific process and environment used by the session runtime.
pub struct ProcessLaunch {
    /// Shell command used to launch the harness.
    pub command: String,
    /// Environment added to the generic agent session environment.
    pub environment: Vec<(String, String)>,
}

/// Harness-neutral inputs of one session launch.
#[derive(Clone, Copy, Debug)]
pub struct LaunchRequest<'a> {
    /// Guest home directory holding the harness configuration.
    pub home: &'a str,
    /// Harness-native conversation to continue instead of starting a fresh one.
    pub resume: Option<&'a str>,
    /// Transcript file the harness reported for the conversation `resume` names, when it did.
    /// A harness whose conversation files live where its own configuration decides finds them
    /// by this path rather than by guessing the location.
    pub resume_transcript: Option<&'a str>,
    /// First prompt of a fresh conversation, passed as the harness's positional
    /// prompt argument so it starts working immediately; ignored when resuming.
    pub initial_prompt: Option<&'a str>,
    /// Model and effort level the session was created with.
    pub model_selection: &'a ModelSelection,
}

/// Resolves the selected harness's terminal launch configuration.
///
/// Each adapter spells the request's model and effort in its own launch
/// vocabulary; both apply to fresh and resumed conversations alike.
#[must_use]
pub fn launch_linux(harness: Harness, request: &LaunchRequest<'_>) -> ProcessLaunch {
    match harness {
        Harness::ClaudeCode => claude_code::launch_linux(request),
        Harness::Codex => codex::launch_linux(request),
        Harness::Pi => pi::launch_linux(request),
    }
}

/// Model every session of `harness` launched with before sessions recorded a
/// model, when the adapter hardcoded one. Persistence records it for existing
/// sessions when it adopts the session selection columns.
pub(crate) const fn model_launched_before_selection(harness: Harness) -> Option<&'static str> {
    match harness {
        Harness::ClaudeCode => Some(claude_code::MODEL_LAUNCHED_BEFORE_SELECTION),
        Harness::Codex | Harness::Pi => None,
    }
}

/// Quotes `value` as one POSIX shell word, safe for any content.
pub(crate) fn shell_single_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Validates an initial prompt before it is persisted for an argv-based launch.
pub(crate) fn validate_initial_prompt(prompt: &str) -> Result<(), Error> {
    if prompt.contains('\0') {
        return Err(Error::Invalid("initial prompt must not contain NUL".into()));
    }
    let quoted_bytes = prompt
        .len()
        .saturating_add(prompt.bytes().filter(|byte| *byte == b'\'').count().saturating_mul(3))
        .saturating_add(2);
    if quoted_bytes > MAX_INITIAL_PROMPT_ARGUMENT_BYTES {
        return Err(Error::Invalid(format!(
            "initial prompt is too large; its encoded launch argument must not exceed {} KiB",
            MAX_INITIAL_PROMPT_ARGUMENT_BYTES / 1024
        )));
    }
    Ok(())
}

/// Recognizes an initialized input line before a harness reports its conversation.
/// The runtime supplies the visible cursor line and pane title.
pub(crate) fn input_ready_without_report(harness: Harness, cursor_line: &str, title: &str) -> bool {
    match harness {
        Harness::ClaudeCode | Harness::Pi => false,
        Harness::Codex => codex::input_ready_without_report(cursor_line, title),
    }
}

/// Parses harness transcript bytes into ordered, runtime-neutral turns.
///
/// # Errors
///
/// Returns an error when the transcript cannot be decoded.
pub(crate) fn parse_transcript(harness: Harness, bytes: &[u8]) -> Result<Vec<crate::sessions::Turn>, Error> {
    match harness {
        Harness::ClaudeCode => claude_code::transcript::parse(bytes),
        Harness::Codex => codex::transcript::parse(bytes),
        Harness::Pi => pi::transcript::parse(bytes),
    }
}

/// Trims a transcript suffix to its first complete harness turn.
pub(crate) fn trim_partial_transcript(harness: Harness, bytes: &[u8]) -> &[u8] {
    match harness {
        Harness::ClaudeCode => claude_code::transcript::trim_partial(bytes),
        Harness::Codex => codex::transcript::trim_partial(bytes),
        Harness::Pi => pi::transcript::trim_partial(bytes),
    }
}

#[cfg(test)]
pub(crate) const fn test_harness() -> Harness {
    Harness::ClaudeCode
}

/// A launch request for tests, with no model or effort selected.
#[cfg(test)]
#[cfg(unix)]
pub(crate) const fn test_launch_request<'a>(
    resume: Option<&'a str>,
    initial_prompt: Option<&'a str>,
) -> LaunchRequest<'a> {
    const UNSELECTED: ModelSelection = ModelSelection {
        model: None,
        effort: None,
    };
    LaunchRequest {
        home: "/home/agent",
        resume,
        resume_transcript: None,
        initial_prompt,
        model_selection: &UNSELECTED,
    }
}

/// Reports whether `flag` is immediately followed by `value` among `arguments`.
#[cfg(test)]
#[cfg(unix)]
pub(crate) fn has_pair(arguments: &[String], flag: &str, value: &str) -> bool {
    arguments.windows(2).any(|pair| pair[0] == flag && pair[1] == value)
}

/// Runs a launch command under `sh` in place of the guest's tmux pane, with a stub `program`
/// on `PATH` that records the arguments it starts with, and returns them, or `None` when the
/// command did not start it.
#[cfg(test)]
#[cfg(unix)]
pub(crate) fn run_launch(program: &str, launch: &ProcessLaunch) -> Option<Vec<String>> {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("temporary directory");
    let stub = directory.path().join(program);
    // `fs::write` closes the stub before it runs.
    std::fs::write(
        &stub,
        "#!/bin/sh\nfor argument in \"$@\"; do printf '%s\\0' \"$argument\"; done > \"$RECORDED_ARGUMENTS\"\n",
    )
    .expect("stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("stub permissions");
    let recorded = directory.path().join("arguments");
    let path = std::env::var("PATH").unwrap_or_default();
    let run = || {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(&launch.command)
            .envs(launch.environment.iter().map(|(name, value)| (name, value)))
            .env("PATH", format!("{}:{path}", directory.path().display()))
            .env("RECORDED_ARGUMENTS", &recorded)
            .output()
            .expect("sh")
    };
    // A process another test thread forks while the stub is being written inherits its write
    // descriptor until that process execs, and executing the stub meanwhile fails with ETXTBSY.
    let mut output = run();
    for _ in 0..100 {
        let busy = output.status.code() == Some(126) && String::from_utf8_lossy(&output.stderr).contains("busy");
        if !busy {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        output = run();
    }
    assert!(
        output.status.success(),
        "{}\n{}",
        launch.command,
        String::from_utf8_lossy(&output.stderr)
    );
    let arguments = std::fs::read(&recorded).ok()?;
    Some(
        String::from_utf8(arguments)
            .expect("UTF-8 arguments")
            .split_terminator('\0')
            .map(str::to_owned)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn claude_code(login: &str) -> HarnessSpec {
        serde_yaml_ng::from_str(&format!("type: claudeCode\nlogin: {login}\n")).expect("installation")
    }

    #[tokio::test(flavor = "local")]
    async fn a_login_never_stored_or_forgotten_names_the_command_that_stores_it() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let database = persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        let installation = claude_code("work");

        assert!(!authentication_ready(&installation, &database).await.expect("readiness"));
        let Err(error) = prepare(&installation, &database).await else {
            panic!("a missing login cannot be mediated");
        };
        assert!(error.to_string().contains("login \"work\" is not stored"), "{error}");
        assert!(error.to_string().contains("`agentctl claude login work`"), "{error}");
        let default = prepare(&claude_code("default"), &database)
            .await
            .err()
            .expect("missing");
        assert!(default.to_string().contains("`agentctl claude login`"), "{default}");
    }

    #[test]
    fn login_names_name_stored_secrets_so_only_plain_names_pass() {
        for name in ["work", "Private-2", "a"] {
            assert!(LoginName::new(name).is_ok(), "{name}");
        }
        for name in ["", "-work", "work-", "work/other", "work.other", "a b", &"x".repeat(65)] {
            assert!(LoginName::new(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn harness_names_match_the_desktop_helpers_screenshot_budget_table() {
        // Sessions get these as AGENT_HARNESS, which the base image's `desktop` helper matches on.
        assert_eq!(Harness::ClaudeCode.as_str(), "claudeCode");
        assert_eq!(Harness::Codex.as_str(), "codex");
        assert_eq!(Harness::Pi.as_str(), "pi");
    }

    #[test]
    fn inside_an_agent_a_login_takes_its_mediated_credentials() {
        let holding = |name: &str| (name == "AGENT_CLAUDE_ACCESS_TOKEN").then(|| "placeholder\n".into());
        let claude = agent_credential(Harness::ClaudeCode, true, holding).expect("Claude Code");
        assert_eq!(claude.as_deref().map(String::as_str), Some("placeholder"));

        // On a host a login signs in, whatever its environment holds.
        assert!(
            agent_credential(Harness::ClaudeCode, false, holding)
                .expect("host")
                .is_none()
        );
        // In an agent without the harness, no sign-in could complete: fail at once instead.
        let error = agent_credential(Harness::ClaudeCode, true, |_: &str| None).expect_err("no login to reuse");
        let error = error.to_string();
        assert!(
            error.contains("no Claude Code login to reuse: `AGENT_CLAUDE_ACCESS_TOKEN` is not set"),
            "{error}"
        );
        assert!(error.contains("manifest must declare Claude Code"), "{error}");
    }

    #[test]
    fn launch_selections_are_opaque_but_command_line_safe() {
        for value in [
            "fable",
            "claude-fable-5",
            "gpt-5.4-codex",
            "us.anthropic.claude-v1:0",
            "org/model@2",
            "xhigh",
        ] {
            assert_eq!(Model::new(value).expect("valid model").as_str(), value);
            assert_eq!(Effort::new(value).expect("valid effort").as_str(), value);
        }
        for value in [
            "",
            " ",
            "fable ",
            "a b",
            "it's",
            "quote\"d",
            "back\\slash",
            "tab\t",
            "ø",
        ] {
            assert!(Model::new(value).is_err(), "{value:?}");
            assert!(Effort::new(value).is_err(), "{value:?}");
        }
        assert!(Model::new("m".repeat(MAX_SELECTION_CHARACTERS)).is_ok());
        assert!(Model::new("m".repeat(MAX_SELECTION_CHARACTERS + 1)).is_err());
        let error = Effort::new("").expect_err("empty effort");
        assert!(error.to_string().contains("effort must be 1-128"));
        assert!(serde_json::from_str::<Model>("\"\"").is_err());
        assert_eq!(
            serde_json::to_string(&Model::new("fable").expect("model")).expect("JSON"),
            "\"fable\""
        );
    }

    #[test]
    fn conflicts_name_only_explicitly_requested_differences() {
        let recorded = ModelSelection {
            model: Some(Model::new("fable").expect("model")),
            effort: None,
        };
        assert_eq!(recorded.conflict_with(&ModelSelection::default()), None);
        assert_eq!(recorded.conflict_with(&recorded), None);
        let other_model = ModelSelection {
            model: Some(Model::new("opus").expect("model")),
            effort: None,
        };
        assert_eq!(
            recorded.conflict_with(&other_model).as_deref(),
            Some("already uses model \"fable\", not \"opus\"")
        );
        let effort_only = ModelSelection {
            model: None,
            effort: Some(Effort::new("max").expect("effort")),
        };
        assert_eq!(
            recorded.conflict_with(&effort_only).as_deref(),
            Some("leaves the effort to the harness default, not \"max\"")
        );
    }

    #[test]
    fn initial_prompt_validation_measures_the_shell_quoted_argument() {
        validate_initial_prompt(&"a".repeat(MAX_INITIAL_PROMPT_ARGUMENT_BYTES - 2)).expect("boundary prompt");

        let expanded = "'".repeat(MAX_INITIAL_PROMPT_ARGUMENT_BYTES / 4);
        assert!(validate_initial_prompt(&expanded).is_err());
        assert!(validate_initial_prompt("before\0after").is_err());
    }
}
