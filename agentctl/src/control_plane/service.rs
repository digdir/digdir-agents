use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    rc::Rc,
};

use ::sandbox::{SandboxPath, mount::Mount};
use ignore::WalkBuilder;

use crate::{Agent, AgentId, Error, MountSpec, definitions::DefinitionSource, progress::ProvisioningState};

use super::{AgentRecord, SharedAgentStore, Wakeup};

/// Desired state supplied by a local API client.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ApplyRequest {
    /// Absolute directory against which local manifest sources are resolved.
    pub source_directory: PathBuf,
    /// Absolute path of the manifest being applied, recorded for discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<PathBuf>,
    /// Absolute path of the file supplying declared manifest values. Defaults to `.env` beside the
    /// manifest; omitted on an update keeps the recorded path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_file: Option<PathBuf>,
    /// Host folders for the manifest's bind mounts, in place of the sources it names or for those
    /// it names none, as for `run`. Omitted on an update keeps the agent's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub binds: Vec<super::BindSource>,
    /// Fail instead of updating when the name already identifies an agent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub create_only: bool,
    /// Agent manifest to store.
    pub agent: Agent,
}

/// Wakes reconciliation after desired state changes.
pub trait Notifier {
    /// Schedules reconciliation without blocking the API request.
    fn notify(&self, id: crate::AgentId);
}

impl Notifier for Wakeup {
    fn notify(&self, id: crate::AgentId) {
        self.notify(id);
    }
}

/// Agent control plane facade for desired-state operations.
pub struct ControlPlane {
    pub(super) store: SharedAgentStore,
    pub(super) notifier: Rc<dyn Notifier>,
    provisioning: ProvisioningState,
    immediate_stops: ImmediateStops,
    volumes: Rc<dyn crate::volumes::VolumeStore>,
    pub(super) bundled: Option<super::bundled::Bundled>,
}

/// Agents whose stop ends their sandbox at once, without letting the guest shut down.
///
/// The Control Plane records the request and the reconciler carries it out, also by cutting
/// short a graceful stop already under way. Each request belongs to the generation that stored
/// the stop, so only a start stored after it can supersede it.
#[derive(Clone, Default)]
pub struct ImmediateStops {
    requested: Rc<RefCell<HashMap<AgentId, u64>>>,
    changed: Rc<tokio::sync::Notify>,
}

impl ImmediateStops {
    /// Asks for the agent's stop stored at `generation` to be immediate, waking a graceful stop
    /// under way.
    pub(crate) fn request(&self, id: AgentId, generation: u64) {
        self.requested.borrow_mut().insert(id, generation);
        self.changed.notify_waiters();
    }

    /// The generation of the stop asked to be immediate, forgetting the request.
    pub(crate) fn take(&self, id: AgentId) -> Option<u64> {
        self.requested.borrow_mut().remove(&id)
    }

    /// Forgets a request that a start stored at `generation` superseded. A pass that read the
    /// agent before its stop was stored sees an older generation and keeps the request.
    pub(crate) fn supersede(&self, id: AgentId, generation: u64) {
        let mut requested = self.requested.borrow_mut();
        if requested.get(&id).is_some_and(|stop| *stop < generation) {
            requested.remove(&id);
        }
    }

    /// Completes, forgetting the request, once the agent's stop is asked to be immediate, and
    /// returns the generation of that stop.
    pub(crate) async fn requested(&self, id: AgentId) -> u64 {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            // Registered before the check, so a request between the two still wakes it.
            changed.as_mut().enable();
            if let Some(generation) = self.take(id) {
                return generation;
            }
            changed.await;
        }
    }
}

impl ControlPlane {
    /// Creates an agent control plane facade.
    #[must_use]
    pub fn new(store: SharedAgentStore, notifier: Rc<dyn Notifier>) -> Self {
        Self {
            store,
            notifier,
            provisioning: ProvisioningState::default(),
            immediate_stops: ImmediateStops::default(),
            volumes: Rc::new(crate::volumes::memory::InMemoryVolumeStore::new()),
            bundled: None,
        }
    }

    /// Runs agents from `definitions`, keeping its copies of them in `home`, agentd's home, which
    /// no folder given for a bind mount may hold or be in.
    #[must_use]
    pub fn with_bundled(mut self, definitions: crate::bundled::Definitions, home: PathBuf) -> Self {
        self.bundled = Some(super::bundled::Bundled::new(definitions, home));
        self
    }

    /// Keeps an agent's selected secret file out of the directories volumes copy, as out of
    /// bind mounts.
    #[must_use]
    pub fn with_volumes(mut self, volumes: Rc<dyn crate::volumes::VolumeStore>) -> Self {
        self.volumes = volumes;
        self
    }

    /// Shares requests for immediate stops with the reconciler that carries them out.
    #[must_use]
    pub fn with_immediate_stops(mut self, immediate_stops: ImmediateStops) -> Self {
        self.immediate_stops = immediate_stops;
        self
    }

    /// Projects the reconciler's in-memory provisioning state onto returned agents.
    #[must_use]
    pub fn with_provisioning(mut self, provisioning: ProvisioningState) -> Self {
        self.provisioning = provisioning;
        self
    }

    /// Stores desired state and returns without waiting for reconciliation.
    ///
    /// # Errors
    ///
    /// Returns an error when the request is invalid, changes an immutable field, conflicts with deletion,
    /// or cannot be stored.
    pub async fn apply(&self, request: ApplyRequest) -> Result<Agent, Error> {
        let given = self.validate_bind_sources(&request.binds).await?;
        // Boxed, as preparing reads the filesystem through several steps that would otherwise
        // make every caller's future as large.
        let prepared = Box::pin(self.prepare_local(
            request.source_directory,
            request.manifest_path,
            request.env_file,
            request.agent,
        ))
        .await?;
        let name = prepared.agent.metadata.name.clone();
        let run_state = prepared.agent.spec.run_state;

        loop {
            let result = match self.store.get_by_name(&name).await {
                Ok(current) => {
                    if request.create_only {
                        return Err(Error::Invalid(format!("an agent named {name:?} already exists")));
                    }
                    if current.agent.metadata.deletion_timestamp.is_some() {
                        return Err(Error::Conflict);
                    }
                    if !prepared.same_definition(&current) {
                        return Err(name_clash(&name, &current));
                    }
                    // An update that gives no folders keeps the agent's, as its mounts cannot change.
                    let given = if request.binds.is_empty() {
                        current
                            .bindings
                            .iter()
                            .map(|(target, source)| super::BindSource {
                                source: source.clone(),
                                target: Some(target.clone()),
                            })
                            .collect()
                    } else {
                        given.clone()
                    };
                    let mut bound = prepared.clone();
                    Box::pin(bound.bind(&given)).await?;
                    let mut desired = bound.agent;
                    validate_immutable_fields(&current, &desired)?;
                    desired.spec.set_run_state(applied_run_state(run_state, Some(&current)));
                    let mut record = AgentRecord {
                        id: current.id,
                        source_directory: prepared.source_directory.clone(),
                        manifest_path: prepared.manifest_path.clone().or_else(|| current.manifest_path.clone()),
                        env_file: prepared.env_file.clone().or_else(|| current.env_file.clone()),
                        agent: desired,
                        // Decided when the agent was created.
                        optional_mount_sources: current.optional_mount_sources.clone(),
                        volume_revisions: current.volume_revisions.clone(),
                        origin: prepared.origin.clone(),
                        lifetime: current.lifetime,
                        bindings: bound.bindings,
                    };
                    self.reject_exposed_secrets(&record).await?;
                    // The folders it was given count too: `run` finds an agent by them.
                    if current.agent.spec == record.agent.spec
                        && current.source_directory == record.source_directory
                        && current.manifest_path == record.manifest_path
                        && current.env_file == record.env_file
                        && current.bindings == record.bindings
                    {
                        self.notifier.notify(current.id);
                        return Ok(self.resource(current));
                    }

                    let expected_generation = current.agent.metadata.generation;
                    record.agent.metadata.generation = expected_generation + 1;
                    record.agent.metadata.restart_timestamp = current.agent.metadata.restart_timestamp;
                    record.agent.status = current.agent.status.clone();
                    self.store
                        .put(record.clone(), expected_generation)
                        .await
                        .map(|()| record)
                }
                Err(Error::NotFound) => {
                    let mut new = prepared.clone();
                    Box::pin(new.bind(&given)).await?;
                    new.agent.spec.set_run_state(applied_run_state(run_state, None));
                    Box::pin(self.create(&new, crate::Lifetime::UntilDeleted)).await
                }
                Err(error) => return Err(error),
            };

            match result {
                Err(Error::Conflict) => {}
                Err(error) => return Err(error),
                Ok(record) => {
                    self.notifier.notify(record.id);
                    return Ok(projected(record));
                }
            }
        }
    }

    /// Rejects desired state that would expose a `.env` file or a selected secret file inside a sandbox.
    ///
    /// Secret files hold the real values that mediation exists to keep out of sandboxes. A bind
    /// mount whose source contains a `.env` file, this agent's selected non-default secret file or
    /// another active agent's selected secret file would hand those values to the guest, so the
    /// combination is refused at apply time. The mounts checked are those the agent's sandbox
    /// gets, with canonical sources: an optional source that has since moved, such as through a
    /// symbolic link, keeps the directory the agent was created with. A volume's copy of a
    /// directory holding the agent's selected secret file is refused too, since any agent may
    /// mount it.
    pub(super) async fn reject_exposed_secrets(&self, record: &AgentRecord) -> Result<(), Error> {
        let desired = &record.agent;
        reject_dot_env_in_bind_mounts(bind_mount_sources(record), "bind-mounts").await?;
        let mut secret_files = Vec::new();
        if !desired.spec.secrets.is_empty() {
            let path = record.env_file_path();
            if record.env_file.is_some() || tokio::fs::try_exists(&path).await? {
                secret_files.push((desired.metadata.name.clone(), canonical_secret_file(&path).await));
            }
        }
        let mut mounts = bind_mount_sources(record)
            .into_iter()
            .map(|(field, source)| (field, "bind-mounts", source))
            .collect::<Vec<_>>();
        if !desired.spec.secrets.is_empty() {
            // A copy any agent may mount must not hold the file, whichever agent selects it.
            for volume in self.volumes.list().await? {
                let crate::volumes::VolumeSpec::Copy { source } = volume.volume.spec;
                let name = format!("Volume {:?}", volume.volume.metadata.name);
                // Agents may still mount revisions of an earlier source.
                let sources = std::iter::once(source)
                    .chain(
                        volume
                            .volume
                            .status
                            .revisions
                            .into_iter()
                            .map(|revision| revision.source),
                    )
                    .collect::<std::collections::BTreeSet<_>>();
                mounts.extend(sources.into_iter().map(|source| (name.clone(), "copies", source)));
            }
        }
        for other in self.store.list().await? {
            if other.id == record.id || other.agent.metadata.deletion_timestamp.is_some() {
                continue;
            }
            if !other.agent.spec.secrets.is_empty() {
                let path = other.env_file_path();
                if other.env_file.is_some() || tokio::fs::try_exists(&path).await? {
                    secret_files.push((other.agent.metadata.name.clone(), canonical_secret_file(&path).await));
                }
            }
            if !desired.spec.secrets.is_empty() {
                mounts.extend(
                    bind_mount_sources(&other)
                        .into_iter()
                        .map(|(_, source)| (format!("Agent {:?}", other.agent.metadata.name), "bind-mounts", source)),
                );
            }
        }
        for (owner, secret_file) in &secret_files {
            for (mount, verb, source) in &mounts {
                if secret_file.starts_with(source) {
                    return Err(Error::Invalid(format!(
                        "{mount} {verb} {} which contains the secret file {} of agent {owner:?}; \
                         the sandbox would see its real values. Keep secret files outside mounted directories, \
                         for example with `agentctl apply --env-file`",
                        source.display(),
                        secret_file.display(),
                    )));
                }
            }
        }
        Ok(())
    }

    /// Gets desired and most recently observed state.
    ///
    /// # Errors
    ///
    /// Returns an error when the agent does not exist or storage fails.
    pub async fn get(&self, name: &str) -> Result<Agent, Error> {
        self.store.get_by_name(name).await.map(|record| self.resource(record))
    }

    /// Lists every active agent ordered by name.
    ///
    /// # Errors
    ///
    /// Returns an error when storage cannot be read.
    pub async fn list(&self) -> Result<Vec<Agent>, Error> {
        self.store
            .list()
            .await
            .map(|records| records.into_iter().map(|record| self.resource(record)).collect())
    }

    /// Reads an agent's stored status and the complete progress of its latest
    /// pass. When `output` names that pass, only later output is included.
    ///
    /// # Errors
    ///
    /// Returns an error when the agent does not exist or storage fails.
    pub async fn progress(
        &self,
        name: &str,
        output: Option<crate::progress::OutputPosition>,
    ) -> Result<(crate::Status, Option<crate::progress::Provisioning>), Error> {
        let record = self.store.get_by_name(name).await?;
        let provisioning = self.provisioning.get(record.id).map(|mut provisioning| {
            if let Some(output) = output.filter(|output| output.pass == provisioning.pass) {
                provisioning.progress = provisioning.progress.output_from(output.sequence);
            }
            provisioning
        });
        Ok((record.agent.status, provisioning))
    }

    /// Resolves the closest agent source directory containing `directory`.
    ///
    /// # Errors
    ///
    /// Returns an error when no agent matches, several are found equally close, or
    /// storage cannot be read.
    pub async fn resolve_directory(&self, directory: &std::path::Path) -> Result<Agent, Error> {
        self.resolve_directory_variant(directory, None).await
    }

    /// Resolves the closest agent associated with `directory`, optionally by
    /// the variant encoded in its recorded leaf manifest filename.
    ///
    /// When several closest agents tie without an explicit variant, exactly one
    /// agent originating from the default `agent.yaml` manifest is preferred.
    ///
    /// # Errors
    ///
    /// Returns an error when no agent matches, selection remains ambiguous, or
    /// storage cannot be read.
    pub async fn resolve_directory_variant(
        &self,
        directory: &std::path::Path,
        variant: Option<&crate::AgentVariantName>,
    ) -> Result<Agent, Error> {
        if !directory.is_absolute() {
            return Err(Error::Invalid("directory must be absolute".into()));
        }
        let directory = canonical_or_original(directory).await;
        let records = self.store.list().await?;
        let definition_sources = definition_sources(&records, &directory).await?;
        let mut matches = Vec::new();
        for (record, definition_source) in records.into_iter().zip(definition_sources) {
            // How close a match is: its depth, then whether it is one of the agent's own
            // directories rather than its definition source, which loses a tie.
            let mut closest: Option<(usize, bool)> = None;
            let sources = association_directories(&record)
                .map(|source| (source, true))
                .chain(definition_source.map(|source| (source, false)));
            for (source, own) in sources {
                let source = canonical_or_original(&source).await;
                if directory.starts_with(&source) {
                    closest = closest.max(Some((source.components().count(), own)));
                }
            }
            if let Some(closeness) = closest {
                matches.push((record, closeness));
            }
        }
        let Some(closest) = matches.iter().map(|(_, closeness)| *closeness).max() else {
            return Err(Error::NotFound);
        };
        matches.retain(|(_, closeness)| *closeness == closest);
        if let Some(variant) = variant {
            let filename = variant.filename();
            matches.retain(|(record, _)| {
                record
                    .manifest_path
                    .as_deref()
                    .and_then(std::path::Path::file_name)
                    .is_some_and(|name| name == filename.as_str())
            });
            if matches.is_empty() {
                return Err(Error::Invalid(format!(
                    "no agent associated with this directory was applied from {filename}"
                )));
            }
        } else if matches.len() > 1
            && matches
                .iter()
                .all(|(record, _)| record.source_directory == matches[0].0.source_directory)
        {
            // Variants of one definition prefer its `agent.yaml`. Agents from different
            // definitions stay ambiguous, as nothing says which one is meant.
            let defaults = matches
                .iter()
                .enumerate()
                .filter_map(|(index, (record, _))| {
                    let filename = record
                        .manifest_path
                        .as_deref()
                        .and_then(std::path::Path::file_name)
                        .or_else(|| Some(std::ffi::OsStr::new(crate::manifest::MANIFEST_FILE)));
                    (filename == Some(std::ffi::OsStr::new(crate::manifest::MANIFEST_FILE))).then_some(index)
                })
                .collect::<Vec<_>>();
            if let [index] = defaults.as_slice() {
                return Ok(self.resource(matches.swap_remove(*index).0));
            }
        }
        if matches.len() != 1 {
            let mut names = matches
                .iter()
                .map(|(record, _)| record.agent.metadata.name.clone())
                .collect::<Vec<_>>();
            names.sort();
            return Err(Error::Invalid(format!(
                "more than one agent is found here ({}); pass --agent NAME or --variant VARIANT",
                names.join(", ")
            )));
        }
        matches
            .pop()
            .map(|(record, _)| self.resource(record))
            .ok_or(Error::NotFound)
    }

    /// Records whether an agent's sandbox runs and returns the agent as stored,
    /// without waiting for the reconciler to stop or start it. A change is a new
    /// generation; setting the run state the agent already has changes nothing.
    ///
    /// # Errors
    ///
    /// Returns an error when the agent does not exist, is being deleted, or
    /// cannot be stored.
    pub async fn set_run_state(&self, name: &str, state: crate::RunState) -> Result<Agent, Error> {
        let record = self
            .change_desired(name, |agent| {
                let changed = agent.spec.run_state() != state;
                agent.spec.set_run_state(state);
                changed
            })
            .await?;
        Ok(self.resource(record))
    }

    /// Records an agent as stopped, as [`Self::set_run_state`] does, and has the reconciler end
    /// its sandbox at once instead of letting the guest shut down.
    ///
    /// # Errors
    ///
    /// Returns an error when the agent does not exist, is being deleted, or cannot be stored.
    pub async fn stop_now(&self, name: &str) -> Result<Agent, Error> {
        let id = self.store.get_by_name(name).await?.id;
        let stopping = self.set_run_state(name, crate::RunState::Stopped).await?;
        // Requested once the stop's generation is known; a graceful stop a pass began meanwhile
        // is cut short by it.
        self.immediate_stops.request(id, stopping.metadata.generation);
        Ok(stopping)
    }

    /// Records a restart of an agent as a new generation and returns the agent as stored, without
    /// waiting for the reconciler to carry it out. The reconciler stops the sandbox as a stop
    /// does, then starts it on the same disk; a stopped agent is started. With `now`, the stop
    /// ends the sandbox at once, as [`Self::stop_now`] does.
    ///
    /// The restart is stored with the run state, so no other write can come between its stop and
    /// its start, and the reconciler finishes it even when the client goes away.
    ///
    /// # Errors
    ///
    /// Returns an error when the agent does not exist, is being deleted, or cannot be stored.
    pub async fn restart(&self, name: &str, now: bool) -> Result<Agent, Error> {
        let requested = time::OffsetDateTime::now_utc();
        let record = self
            .change_desired(name, |agent| {
                agent.spec.set_run_state(crate::RunState::Running);
                agent.metadata.restart_timestamp = Some(requested);
                true
            })
            .await?;
        if now {
            self.immediate_stops
                .request(record.id, record.agent.metadata.generation);
        }
        Ok(self.resource(record))
    }

    /// Stores `change` to an agent's desired state as a new generation, retrying it on a
    /// concurrent write, wakes the reconciler and returns the agent as stored. When `change`
    /// reports that it changed nothing, nothing is stored and the reconciler is only woken.
    async fn change_desired(&self, name: &str, change: impl Fn(&mut Agent) -> bool) -> Result<AgentRecord, Error> {
        loop {
            let mut record = self.store.get_by_name(name).await?;
            if record.agent.metadata.deletion_timestamp.is_some() {
                return Err(Error::Conflict);
            }
            if change(&mut record.agent) {
                let expected_generation = record.agent.metadata.generation;
                record.agent.metadata.generation = expected_generation + 1;
                match self.store.put(record.clone(), expected_generation).await {
                    Ok(()) => {}
                    Err(Error::Conflict) => continue,
                    Err(error) => return Err(error),
                }
            }
            self.notifier.notify(record.id);
            return Ok(record);
        }
    }

    /// Marks an agent for asynchronous release. Repeated deletion is safe.
    ///
    /// # Errors
    ///
    /// Returns an error when the deletion marker cannot be stored.
    pub async fn delete(&self, name: &str) -> Result<(), Error> {
        match self.store.mark_deleting(name).await {
            Ok(record) => {
                self.notifier.notify(record.id);
                Ok(())
            }
            Err(Error::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Rejects any bind source containing a `.env` file, case-insensitively and regardless of ignores.
///
/// Filesystem traversal is blocking and may cover a whole checkout, so it stays off the local
/// async runtime. Directories named `.env` are allowed. Symbolic links are not followed, but a
/// link itself named `.env` is rejected. A source that no longer exists, as an optional one may
/// not, exposes nothing.
async fn reject_dot_env_in_bind_mounts(mounts: Vec<(String, PathBuf)>, verb: &'static str) -> Result<(), Error> {
    tokio::task::spawn_blocking(move || {
        for (field, source) in mounts {
            if matches!(source.try_exists(), Ok(false)) {
                continue;
            }
            for result in WalkBuilder::new(&source)
                .hidden(false)
                .ignore(false)
                .git_ignore(false)
                .git_global(false)
                .git_exclude(false)
                .parents(false)
                .follow_links(false)
                .build()
            {
                let entry = result.map_err(|error| {
                    Error::Invalid(format!(
                        "cannot inspect {field}.source {} for .env files: {error}",
                        source.display()
                    ))
                })?;
                let is_directory = entry.file_type().is_some_and(|kind| kind.is_dir());
                let is_env_file = entry
                    .file_name()
                    .as_encoded_bytes()
                    .eq_ignore_ascii_case(super::resource::ENV_FILE.as_bytes());
                if !is_directory && is_env_file {
                    return Err(Error::Invalid(format!(
                        "{field} {verb} {} which contains .env at {}; the sandbox would see its real values. \
                         Remove the file or keep it outside mounted directories",
                        source.display(),
                        entry.path().display(),
                    )));
                }
            }
        }
        Ok(())
    })
    .await
    .map_err(|error| Error::Daemon(format!("bind-mount .env inspection failed: {error}")))?
}

impl ControlPlane {
    /// Converts a stored record to its API representation, projecting
    /// provisioning progress, provenance and volume revisions into status.
    pub(super) fn resource(&self, record: AgentRecord) -> Agent {
        let progress = self.provisioning.summary(record.id);
        let mut agent = projected(record);
        agent.status.progress = progress;
        agent
    }
}

/// The agent a stored record holds, with its origin, provenance and the revisions of the volumes it
/// mounts projected into status.
pub(super) fn projected(record: AgentRecord) -> Agent {
    let mut agent = record.agent;
    agent.status.volumes = agent
        .spec
        .sandbox
        .mounts
        .iter()
        .filter_map(|mount| match mount {
            crate::MountSpec::Volume { volume, target, .. } => Some(crate::MountedVolume {
                target: target.clone(),
                volume: volume.clone(),
                revision: record.volume_revisions.get(target)?.as_ref().map(|pin| pin.revision),
            }),
            _ => None,
        })
        .collect();
    agent.status.provenance = Some(crate::Provenance {
        source_directory: record.source_directory,
        manifest_path: record.manifest_path,
        env_file: record.env_file,
    });
    agent.status.origin = Some(record.origin);
    agent.status.lifetime = (record.lifetime != crate::Lifetime::UntilDeleted).then_some(record.lifetime);
    agent.status.bindings = record.bindings;
    agent
}

/// The run state an apply stores: the manifest's, else the agent's own, so
/// applying a manifest that omits it never stops or starts the agent.
pub(super) fn applied_run_state(requested: Option<crate::RunState>, current: Option<&AgentRecord>) -> crate::RunState {
    requested
        .or_else(|| current.map(|current| current.agent.spec.run_state()))
        .unwrap_or_default()
}

pub(super) fn validate_immutable_fields(current: &AgentRecord, desired: &Agent) -> Result<(), Error> {
    if current.agent.spec.sandbox.image != desired.spec.sandbox.image {
        return Err(Error::Immutable("spec.sandbox.image"));
    }
    if current.agent.spec.sandbox.platform != desired.spec.sandbox.platform {
        return Err(Error::Immutable("spec.sandbox.platform"));
    }
    if current.agent.spec.sandbox.init_system != desired.spec.sandbox.init_system {
        return Err(Error::Immutable("spec.sandbox.initSystem"));
    }
    if current.agent.spec.sandbox.resources.root_filesystem.mode()
        != desired.spec.sandbox.resources.root_filesystem.mode()
    {
        return Err(Error::Immutable("spec.sandbox.resources.rootFilesystem.mode"));
    }
    if current.agent.spec.sandbox.mounts != desired.spec.sandbox.mounts {
        return Err(Error::Immutable("spec.sandbox.mounts"));
    }
    if current.agent.spec.home != desired.spec.home {
        return Err(Error::Immutable("spec.home"));
    }
    let current_kinds = current
        .agent
        .spec
        .harnesses
        .iter()
        .map(|harness| harness.kind)
        .collect::<std::collections::BTreeSet<_>>();
    let desired_kinds = desired
        .spec
        .harnesses
        .iter()
        .map(|harness| harness.kind)
        .collect::<std::collections::BTreeSet<_>>();
    if current_kinds != desired_kinds {
        return Err(Error::Immutable("spec.harnesses.type"));
    }
    let current_auth = current
        .agent
        .spec
        .harnesses
        .iter()
        .map(|harness| (harness.kind, harness.auth))
        .collect::<std::collections::BTreeSet<_>>();
    let desired_auth = desired
        .spec
        .harnesses
        .iter()
        .map(|harness| (harness.kind, harness.auth))
        .collect::<std::collections::BTreeSet<_>>();
    if current_auth != desired_auth {
        return Err(Error::Immutable("spec.harnesses.auth"));
    }
    Ok(())
}

/// Another definition's agent has the name: nothing about this agent changed, so it is not an immutable field.
fn name_clash(name: &str, other: &AgentRecord) -> Error {
    let from = match &other.origin {
        crate::Origin::Reference { reference, .. } => format!("from definition {reference:?}"),
        crate::Origin::Local => format!("from {}", other.source_directory.display()),
    };
    Error::Invalid(format!(
        "an agent named {name:?} was already applied {from}; choose another name in metadata.name or pass `--name`"
    ))
}

/// Resolves each bind mount's source, the one `given` for it or else the manifest's, returning the
/// canonical sources of the optional ones that exist, and the given ones that differ from the
/// manifest's, by target. The `given` sources are canonical.
///
/// A required source is replaced by its canonical form, so the sandbox keeps the directory it was
/// created with. An optional one keeps the path it declares, so that applying the same manifest
/// again compares equal whether the directory has since appeared or disappeared.
pub(super) async fn resolve_mount_sources(
    agent: &mut Agent,
    source_directory: &std::path::Path,
    given: &[super::BindSource],
) -> Result<(BTreeMap<SandboxPath, PathBuf>, BTreeMap<SandboxPath, PathBuf>), Error> {
    let mut chosen = given_sources(&agent.spec.sandbox.mounts, given)?;
    let mut optional_sources = BTreeMap::new();
    let mut bindings = BTreeMap::new();
    for (index, mount) in agent.spec.sandbox.mounts.iter_mut().enumerate() {
        let MountSpec::Bind {
            source,
            target,
            optional,
            ..
        } = mount
        else {
            continue;
        };
        let field = format!("spec.sandbox.mounts[{index}].source");
        let declared = source
            .as_deref()
            .map(|source| crate::manifest::resolve_host_path(source_directory, source, &field))
            .transpose()?;
        let unresolved = match (chosen.remove(&index), declared) {
            (Some(given), declared) => {
                // A folder that is the manifest's own source is not one the agent was given.
                let declared = match declared {
                    Some(declared) => Some(canonical_or_original(&declared).await),
                    None => None,
                };
                if declared.as_ref() != Some(&given) {
                    bindings.insert(target.clone(), given.clone());
                }
                given
            }
            (None, Some(declared)) => declared,
            (None, None) => {
                return Err(Error::Invalid(format!(
                    "spec.sandbox.mounts[{index}] names no source, so it needs a folder for {}: \
                     give one with `--mount FOLDER`, or as the folder of `agentctl run DEFINITION FOLDER`",
                    target.as_str()
                )));
            }
        };
        if *optional {
            *source = Some(unresolved.clone());
            let present = tokio::fs::try_exists(&unresolved).await.map_err(|error| {
                Error::Invalid(format!("{field} {} cannot be checked: {error}", unresolved.display()))
            })?;
            if !present {
                continue;
            }
        }
        let resolved = tokio::fs::canonicalize(&unresolved)
            .await
            .map_err(|error| Error::Invalid(format!("{field} {} cannot be resolved: {error}", unresolved.display())))?;
        if !tokio::fs::metadata(&resolved).await?.is_dir() {
            return Err(Error::Invalid(format!(
                "{field} {} must identify a directory",
                resolved.display()
            )));
        }
        if *optional {
            optional_sources.insert(target.clone(), resolved);
            continue;
        }
        *source = Some(resolved);
    }
    Ok((optional_sources, bindings))
}

/// Which bind mount, by its index, each of the `given` sources is for: the one with its target, or
/// the one without a source.
fn given_sources(mounts: &[MountSpec], given: &[super::BindSource]) -> Result<BTreeMap<usize, PathBuf>, Error> {
    let mut chosen = BTreeMap::new();
    for bind in given {
        let index = if let Some(target) = &bind.target {
            mounts
                .iter()
                .position(|mount| matches!(mount, MountSpec::Bind { target: bound, .. } if bound == target))
                .ok_or_else(|| Error::Invalid(format!("the manifest has no bind mount at {}", target.as_str())))?
        } else {
            let unsourced = mounts
                .iter()
                .enumerate()
                .filter(|(_, mount)| matches!(mount, MountSpec::Bind { source: None, .. }))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            match unsourced.as_slice() {
                [index] => *index,
                [] => {
                    return Err(Error::Invalid(format!(
                        "the manifest has no bind mount without a source for {} to fill; name the target \
                         of the one it replaces, as `--mount SOURCE:TARGET`",
                        bind.source.display()
                    )));
                }
                _ => {
                    return Err(Error::Invalid(format!(
                        "the manifest has {} bind mounts without a source; name each one's target, as \
                         `--mount SOURCE:TARGET`",
                        unsourced.len()
                    )));
                }
            }
        };
        if chosen.insert(index, bind.source.clone()).is_some() {
            return Err(Error::Invalid(format!(
                "two folders are given for spec.sandbox.mounts[{index}]"
            )));
        }
    }
    Ok(chosen)
}

pub(super) fn validate_request_paths(
    source_directory: &std::path::Path,
    manifest_path: Option<&std::path::Path>,
    env_file: Option<&std::path::Path>,
) -> Result<(), Error> {
    if !source_directory.is_absolute() {
        return Err(Error::Invalid("sourceDirectory must be absolute".into()));
    }
    if let Some(manifest) = manifest_path
        && manifest.parent() != Some(source_directory)
    {
        return Err(Error::Invalid(
            "manifestPath must name a file in sourceDirectory".into(),
        ));
    }
    if let Some(env_file) = env_file
        && !env_file.is_absolute()
    {
        return Err(Error::Invalid("envFile must be absolute".into()));
    }
    Ok(())
}

/// The canonical source of each bind mount the agent's sandbox gets, named by its field.
fn bind_mount_sources(record: &AgentRecord) -> Vec<(String, PathBuf)> {
    record
        .indexed_sandbox_mounts()
        .filter_map(|(index, mount)| match mount {
            Mount::Bind { source, .. } => Some((format!("spec.sandbox.mounts[{index}]"), source)),
            _ => None,
        })
        .collect()
}

async fn canonical_or_original(path: &std::path::Path) -> PathBuf {
    tokio::fs::canonicalize(path)
        .await
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Rejects copying the host directory `source`, named by `field`, into sandboxes when it holds an
/// active agent's selected secret file, whose real values the sandboxes would see. `source` is
/// canonical. A `.env` file in it is found while the directory is fingerprinted.
///
/// # Errors
///
/// Returns [`Error::Invalid`] naming the file, or an error when the agents cannot be read.
pub(crate) async fn reject_secrets_in_copy(
    agents: &dyn super::AgentStore,
    field: &str,
    source: &std::path::Path,
) -> Result<(), Error> {
    for agent in agents.list().await? {
        if agent.agent.metadata.deletion_timestamp.is_some() || agent.agent.spec.secrets.is_empty() {
            continue;
        }
        let path = agent.env_file_path();
        if agent.env_file.is_none() && !tokio::fs::try_exists(&path).await? {
            continue;
        }
        let secret_file = canonical_secret_file(&path).await;
        if secret_file.starts_with(source) {
            return Err(Error::Invalid(format!(
                "{field} copies {} which contains the secret file {} of agent {:?}; the sandboxes would see its \
                 real values. Keep secret files outside copied directories, for example with `agentctl apply --env-file`",
                source.display(),
                secret_file.display(),
                agent.agent.metadata.name,
            )));
        }
    }
    Ok(())
}

/// Canonical form of a secret file for comparison against canonical bind mount sources.
///
/// The file, and any number of its parent directories, may not exist yet. The nearest existing
/// ancestor is canonicalized and the missing components appended, so a symlink anywhere above
/// the file still compares equal to the resolved mount source.
async fn canonical_secret_file(path: &std::path::Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut ancestor = path;
    loop {
        if let Ok(canonical) = tokio::fs::canonicalize(ancestor).await {
            return missing
                .iter()
                .rev()
                .fold(canonical, |joined, component| joined.join(component));
        }
        match (ancestor.parent(), ancestor.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name);
                ancestor = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The definition source of each agent in `records`, as [`crate::definitions::source`] finds it, when the agent is
/// found from `directory` through it: from anywhere in a repository, but from a folder holding `agents/` only when
/// `directory` is that folder.
///
/// Finding a source reads the filesystem, so it stays off the local async runtime.
async fn definition_sources(
    records: &[AgentRecord],
    directory: &std::path::Path,
) -> Result<Vec<Option<PathBuf>>, Error> {
    let sources = records
        .iter()
        .map(|record| record.source_directory.clone())
        .collect::<Vec<_>>();
    let directory = directory.to_path_buf();
    tokio::task::spawn_blocking(move || {
        sources
            .iter()
            .map(|source| match crate::definitions::source(source)? {
                DefinitionSource::Repository(root) => Some(root.to_path_buf()),
                DefinitionSource::Folder(folder) => {
                    let folder = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
                    (directory == folder).then_some(folder)
                }
            })
            .collect()
    })
    .await
    .map_err(|error| Error::Daemon(format!("finding the agents' definition sources failed: {error}")))
}

fn association_directories(record: &AgentRecord) -> impl Iterator<Item = PathBuf> {
    std::iter::once(record.source_directory.clone())
        .chain(bind_mount_sources(record).into_iter().map(|(_, source)| source))
}
