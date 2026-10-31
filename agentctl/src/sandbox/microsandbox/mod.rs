//! Microsandbox integration for the agent layer.

use std::{path::Path, rc::Rc};

use ::sandbox::{EnsureSandboxRequest, ErrorKind, LocalFuture, Platform, SandboxHandle, SandboxService, SandboxState};
use sandbox_microsandbox::{MicrosandboxNetworkBackend, MicrosandboxProvider};

use crate::{Error, authorization::AgentPolicyEngine, control_plane::AgentRecord, persistence};

mod execution;
mod forward;
mod preparation;
mod terminal;

pub(super) use execution::start_execution;
pub(super) use forward::guest_tcp_dialer;
pub use forward::{GuestConnection, GuestDialer};
pub use terminal::attach_terminal;

use preparation::Preparation;

use super::{NestedSandboxesOutcome, Provider, ProviderEnsureOutcome, ProviderId, resolve_nested_sandboxes};

/// `RUST_LOG` directives that keep this Provider's runtime helper processes quiet at the default level.
pub const LOG_DIRECTIVES: &str = sandbox_microsandbox::LOG_DIRECTIVES;

pub(super) const PROVIDER_ID: &str = "microsandbox";

/// How long `agentd` keeps an image no agent uses after its last use, so an agent deleted and
/// applied again, even after a weekend, does not download its image again.
const UNUSED_IMAGE_RETENTION: std::time::Duration = std::time::Duration::from_hours(72);

/// How often an idle `agentd` removes unused images.
const UNUSED_IMAGE_SWEEP: std::time::Duration = std::time::Duration::from_hours(1);

/// Removes unused images while `agentd` runs, so they go even when no agent changes.
async fn remove_unused_images_periodically(provider: std::rc::Weak<MicrosandboxProvider>) {
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + UNUSED_IMAGE_SWEEP, UNUSED_IMAGE_SWEEP);
    loop {
        ticker.tick().await;
        let Some(provider) = provider.upgrade() else {
            return;
        };
        provider.remove_unused_images().await;
    }
}

/// Sandbox-resolvable name of the Microsandbox Network Backend's host alias.
///
/// The Backend's DNS answers this name with the per-sandbox gateway address
/// and rewrites gateway-bound connections to host loopback at dial time, so
/// this is how processes inside a sandbox reach the Platform API endpoint.
pub const HOST_ALIAS: &str = "host.microsandbox.internal";

/// Runtime-selectable agent adapter for a Microsandbox Provider.
pub struct Adapter {
    id: ProviderId,
    service: SandboxService,
    preparation: Preparation,
    default_architecture: String,
    platform_port: u16,
}

impl Adapter {
    /// Opens one configured Microsandbox Provider and its mediated Network Backend.
    ///
    /// # Errors
    ///
    /// Returns an error when Provider state cannot be opened.
    pub async fn open(
        home: &Path,
        database: persistence::Database,
        credentials: Rc<crate::harness::AuthenticationManager>,
        policy: Rc<AgentPolicyEngine>,
        platform_port: u16,
    ) -> Result<Self, Error> {
        let network = Rc::new(MicrosandboxNetworkBackend::new(policy.clone()).with_secret_store(credentials.clone()));
        let service = {
            let provider = Rc::new(
                MicrosandboxProvider::builder(home.join("microsandbox"))
                    .remove_unused_images_after(UNUSED_IMAGE_RETENTION)
                    .open()
                    .await?,
            );
            tokio::task::spawn_local(remove_unused_images_periodically(Rc::downgrade(&provider)));
            SandboxService::new(provider)
        }
        .with_network_backend(network.clone());
        policy.set_platform_endpoint(HOST_ALIAS, platform_port);
        Ok(Self {
            id: ProviderId::new(PROVIDER_ID)?,
            service,
            preparation: Preparation::new(database, credentials, policy, network),
            default_architecture: Platform::native("linux").architecture,
            platform_port,
        })
    }

    /// The Platform API base URL reachable from this Provider's sandboxes.
    #[must_use]
    pub fn platform_url(&self) -> String {
        format!("http://{HOST_ALIAS}:{}", self.platform_port)
    }

    fn sandbox_spec(&self, record: &AgentRecord) -> Result<::sandbox::SandboxSpec, Error> {
        record
            .agent
            .spec
            .sandbox
            .resolve_from(&record.source_directory, &self.default_architecture)
    }

    /// Reports whether the Provider offers nested sandboxes for the agent's platform, and why not
    /// when it does not.
    async fn nested_sandboxes_offered(&self, record: &AgentRecord) -> Result<Result<(), String>, Error> {
        let capabilities = self.service.capabilities(&self.sandbox_spec(record)?.platform).await?;
        if capabilities
            .features()
            .contains(::sandbox::SandboxFeature::NestedSandboxes)
        {
            return Ok(Ok(()));
        }
        Ok(Err(sandbox_microsandbox::nested_sandbox_support()
            .err()
            .unwrap_or_else(|| "this platform has no nested sandboxes".into())))
    }

    fn sandbox_mounts(record: &AgentRecord) -> Vec<::sandbox::mount::Mount> {
        record.agent.spec.sandbox.resolved_mounts()
    }

    /// Prepares `record`'s sandbox and starts it, once its nested sandboxes are resolved.
    async fn ensure_prepared(
        &self,
        record: &AgentRecord,
        mut environment: std::collections::BTreeMap<String, String>,
        nested_sandboxes: Option<NestedSandboxesOutcome>,
        progress: ::sandbox::ProgressReporter,
    ) -> Result<ProviderEnsureOutcome, Error> {
        let prepared = self.preparation.prepare(record).await?;
        let harnesses = prepared.harnesses;
        let logins = prepared.logins;
        for (name, value) in prepared.environment {
            if environment.insert(name.clone(), value).is_some() {
                return Err(Error::Invalid(format!(
                    "Sandbox environment variable {name:?} collides with a mediated secret"
                )));
            }
        }
        let sandbox_name = record.sandbox_name()?;
        let mut spec = self.sandbox_spec(record)?;
        spec.resources = spec
            .resources
            .with_nested_sandboxes(nested_sandboxes == Some(NestedSandboxesOutcome::Available));
        // Ensure starts a stopped sandbox, and restarts a running one to replace its environment
        // or its resources.
        let runtime_restarted = match self.service.inspect(&sandbox_name).await {
            Ok(sandbox) => {
                sandbox.state == SandboxState::Stopped
                    || sandbox.environment != environment
                    || sandbox.resources != spec.resources
            }
            Err(error) if error.is_not_found() => false,
            Err(error) => return Err(error.into()),
        };
        let request = EnsureSandboxRequest::new(sandbox_name, spec)
            .with_hostname(record.sandbox_hostname()?)
            .with_mounts(Self::sandbox_mounts(record))
            .with_environment(environment);
        let mut sandbox = self.service.ensure(&request).forward(&progress).await?;
        if self.preparation.network_is_stale(sandbox.id()) {
            self.preparation.restart_network(&sandbox).await?;
            // Re-ensure starts the stopped Network with the replacement handshake bindings.
            sandbox = self.service.ensure(&request).forward(&progress).await?;
        }
        Ok(ProviderEnsureOutcome {
            sandbox,
            runtime_restarted,
            harnesses,
            logins,
            nested_sandboxes,
        })
    }
}

impl Provider for Adapter {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn supports<'a>(&'a self, record: &'a AgentRecord) -> LocalFuture<'a, Result<bool, Error>> {
        Box::pin(async move {
            match self.service.capabilities(&self.sandbox_spec(record)?.platform).await {
                Ok(capabilities) => Ok(Self::sandbox_mounts(record)
                    .iter()
                    .all(|mount| capabilities.mount_kinds().contains(mount.kind()))),
                Err(error) if error.kind() == ErrorKind::Unsupported => Ok(false),
                Err(error) => Err(error.into()),
            }
        })
    }

    fn ensure<'a>(
        &'a self,
        record: &'a AgentRecord,
        environment: std::collections::BTreeMap<String, String>,
        progress: ::sandbox::ProgressReporter,
    ) -> LocalFuture<'a, Result<ProviderEnsureOutcome, Error>> {
        Box::pin(async move {
            let requested = record.agent.spec.sandbox.resources.nested_sandboxes;
            let offered = match requested {
                Some(_) => self.nested_sandboxes_offered(record).await?,
                None => Ok(()),
            };
            let nested_sandboxes = resolve_nested_sandboxes(requested, offered)?;
            let outcome = self
                .ensure_prepared(record, environment, nested_sandboxes, progress)
                .await;
            // The sandbox may not run with the logins this pass chose; its status keeps those it last ran with.
            if outcome.is_err() {
                self.preparation.release_logins(record);
            }
            outcome
        })
    }

    fn stop<'a>(&'a self, record: &'a AgentRecord) -> LocalFuture<'a, Result<(), Error>> {
        Box::pin(async move { self.service.stop(&record.sandbox_name()?).await.map_err(Error::from) })
    }

    fn kill<'a>(&'a self, record: &'a AgentRecord) -> LocalFuture<'a, Result<(), Error>> {
        Box::pin(async move { self.service.kill(&record.sandbox_name()?).await.map_err(Error::from) })
    }

    fn open<'a>(
        &'a self,
        record: &'a AgentRecord,
        id: &'a ::sandbox::SandboxId,
    ) -> LocalFuture<'a, Result<SandboxHandle, Error>> {
        Box::pin(async move {
            self.service
                .open(id, record.agent.spec.sandbox.resolved_retention_policy())
                .await
                .map_err(Error::from)
        })
    }

    fn release<'a>(&'a self, record: &'a AgentRecord) -> LocalFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            let name = record.sandbox_name()?;
            self.service
                .release(&name, record.agent.spec.sandbox.resolved_retention_policy())
                .await?;
            self.preparation.remove(&name);
            Ok(())
        })
    }
}
