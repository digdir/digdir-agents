//! Running an agent from a definition: one agentd resolves from a reference, or one on disk. Running
//! finds or makes the agent in one request, mounts the workspace it is named for, and can tie its
//! life to the client that asked for it.

use std::path::{Path, PathBuf};

use ::sandbox::SandboxPath;

use crate::{
    Agent, AgentId, DefinitionReference, Error, Lifetime, MountSpec, Origin, WorkspaceSpec,
    bundled::{Definitions, Snapshots},
};

use super::{AgentRecord, ControlPlane, service};

/// A request to run an agent from a definition.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunRequest {
    /// The definition to run.
    pub definition: RunDefinition,
    /// An absolute host folder to mount as the agent's workspace, for a definition with
    /// `spec.workspace`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    /// How long agentd keeps the agent; until it is deleted when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifetime: Option<Lifetime>,
    /// The agent's name. agentd chooses one from the definition and workspace when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The definition a [`RunRequest`] runs.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RunDefinition {
    /// A definition agentd resolves from a reference, as an image is named; see
    /// [`DefinitionReference`]. Today agentd resolves the ones it carries in its build, such as
    /// `bundled/agent-builder`, or `agent-builder` for short.
    Reference {
        /// The reference, which agentd records in its full form as the agent's [`Origin`].
        reference: String,
    },
    /// A manifest on this machine, as the client read it, as for `apply`.
    Local {
        /// Absolute directory against which the manifest's sources are resolved.
        source_directory: PathBuf,
        /// Absolute path of the manifest, which with the workspace identifies the agent.
        manifest_path: PathBuf,
        /// Absolute path of the file supplying declared manifest values, when not `.env` beside it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env_file: Option<PathBuf>,
        /// The agent the manifest declares.
        agent: Box<Agent>,
    },
}

/// The agent a [`RunRequest`] found or made, and what agentd did to it.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Ran {
    /// The agent as stored, which runs once the reconciler has started it.
    pub agent: Agent,
    /// What agentd did to it.
    pub outcome: RunOutcome,
}

/// What [`ControlPlane::run`] did to the agent it returns.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RunOutcome {
    /// It made the agent.
    Created,
    /// It found the agent, made from the definition as it is now, and started it if it was stopped.
    Resumed,
    /// It found the agent, made from an earlier version of the definition, and changed it in place,
    /// starting it if it was stopped.
    Updated,
    /// It found the agent, made from an earlier version of the definition, which changes something
    /// an agent cannot change in place. It left the agent as it was: deleting it and running the
    /// definition again makes it anew.
    Outdated {
        /// What cannot change in place.
        reason: String,
    },
}

/// A definition agentd can run by reference, as it keeps it on disk, where it is applied from as
/// any other.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DefinitionSummary {
    /// The reference that runs it, in its full form, such as `bundled/agent-builder`.
    pub reference: String,
    /// The SHA-256 digest of its files.
    pub digest: String,
    /// The folder holding agentd's copy of it, which is never changed.
    pub path: PathBuf,
    /// The workspace it declares, which running it needs one for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceSpec>,
}

/// The definitions agentd carries, and where it keeps their copies.
pub(super) struct Bundled {
    definitions: Definitions,
    snapshots: Snapshots,
    home: PathBuf,
}

impl Bundled {
    pub(super) const fn new(definitions: Definitions, snapshots: Snapshots, home: PathBuf) -> Self {
        Self {
            definitions,
            snapshots,
            home,
        }
    }
}

/// An agent as a definition declares it, ready to store, before it has a name or a workspace.
pub(super) struct Prepared {
    pub(super) agent: Agent,
    pub(super) source_directory: PathBuf,
    pub(super) manifest_path: Option<PathBuf>,
    pub(super) env_file: Option<PathBuf>,
    pub(super) optional_mount_sources: std::collections::BTreeMap<SandboxPath, PathBuf>,
    pub(super) origin: Origin,
}

impl Prepared {
    /// Whether `record` was made from the same definition, which an agent's definition can never
    /// change to another: the same reference, to any version, or the same folder.
    pub(super) fn same_definition(&self, record: &AgentRecord) -> bool {
        match (&self.origin, &record.origin) {
            (Origin::Reference { reference, .. }, Origin::Reference { reference: other, .. }) => reference == other,
            (Origin::Local, Origin::Local) => self.source_directory == record.source_directory,
            _ => false,
        }
    }

    /// The name of the definition, for an agent named after it.
    fn base_name(&self) -> &str {
        match &self.origin {
            Origin::Reference { reference, .. } => reference.rsplit('/').next().unwrap_or(reference),
            Origin::Local => &self.agent.metadata.name,
        }
    }
}

impl ControlPlane {
    /// Runs an agent from a definition and starts it.
    ///
    /// An agent kept until deleted is the one agent for its definition and workspace, so running
    /// the definition again resumes it, and brings it to the definition as it is now when it can.
    /// An ephemeral one belongs to the request alone, so each request makes its own, which agentd
    /// deletes once the client that asked for it is gone. agentd names the agent after the
    /// definition and the workspace unless the request names it.
    ///
    /// # Errors
    ///
    /// Returns an error when the definition is invalid or unknown, the workspace is refused, the
    /// name belongs to another agent, the agent is being deleted, or it cannot be stored.
    pub async fn run(&self, request: RunRequest) -> Result<(Ran, AgentId), Error> {
        let lifetime = request.lifetime.unwrap_or_default();
        let workspace = match &request.workspace {
            Some(path) => Some(self.validate_workspace(path).await?),
            None => None,
        };
        // Held until the agent is stored, so cleanup cannot remove the copy of its definition first.
        // It also has requests find or make their agents one at a time. Two that race anyway, with
        // an apply between them, cannot both make the agent, since the store takes a name once.
        let snapshots = self.bundled.as_ref().map(|bundled| &bundled.snapshots);
        let guard = match snapshots {
            Some(snapshots) => Some(snapshots.lock().await),
            None => None,
        };
        let mut prepared = match request.definition {
            RunDefinition::Reference { reference } => self.prepare_reference(&reference).await?,
            RunDefinition::Local {
                source_directory,
                manifest_path,
                env_file,
                agent,
            } => {
                self.prepare_local(source_directory, Some(manifest_path), env_file, *agent)
                    .await?
            }
        };
        prepared.agent.spec.set_run_state(crate::RunState::Running);
        prepared
            .agent
            .spec
            .sandbox
            .mounts
            .extend(workspace_mounts(&prepared.agent, workspace.as_deref()).await?);

        let ran = loop {
            let records = self.store.list().await?;
            let keeps = |record: &&AgentRecord| {
                record.lifetime == Lifetime::UntilDeleted
                    && record.workspace == workspace
                    && prepared.same_definition(record)
                    // Variants share a folder but are different definitions. The manifest of a
                    // definition run by reference moves with each version of it.
                    && (!prepared.origin.is_local() || record.manifest_path == prepared.manifest_path)
            };
            let current = match &request.name {
                Some(name) => match records.iter().find(|record| record.agent.metadata.name == *name) {
                    Some(record) if lifetime == Lifetime::UntilDeleted && keeps(&record) => Some(record),
                    Some(_) => return Err(Error::Invalid(format!("an agent named {name:?} already exists"))),
                    None => None,
                },
                None if lifetime == Lifetime::UntilDeleted => records.iter().find(keeps),
                None => None,
            };
            let result = if let Some(current) = current {
                self.resume(current, &prepared).await
            } else {
                prepared.agent.metadata.name = match &request.name {
                    Some(name) => name.clone(),
                    None => available_name(prepared.base_name(), workspace.as_deref(), &records)?,
                };
                prepared.agent.validate()?;
                let mut record = AgentRecord {
                    id: AgentId::generate(),
                    source_directory: prepared.source_directory.clone(),
                    manifest_path: prepared.manifest_path.clone(),
                    env_file: prepared.env_file.clone(),
                    agent: prepared.agent.clone(),
                    optional_mount_sources: prepared.optional_mount_sources.clone(),
                    volume_revisions: std::collections::BTreeMap::new(),
                    origin: prepared.origin.clone(),
                    lifetime,
                    workspace: workspace.clone(),
                };
                record.agent.metadata.generation = 1;
                self.reject_exposed_secrets(&record).await?;
                self.store
                    .put(record.clone(), 0)
                    .await
                    .map(|()| (record, RunOutcome::Created))
            };
            match result {
                // Another request stored the agent or took its name first.
                Err(Error::Conflict) => {}
                result => break result?,
            }
        };
        drop(guard);

        let (record, outcome) = ran;
        self.notifier.notify(record.id);
        if outcome == RunOutcome::Updated {
            self.remove_unused_definitions().await;
        }
        let id = record.id;
        Ok((
            Ran {
                agent: self.resource(record),
                outcome,
            },
            id,
        ))
    }

    /// The definitions agentd can run by reference, each in the folder agentd keeps its copy in,
    /// written if missing.
    ///
    /// # Errors
    ///
    /// Returns an error when a copy cannot be written.
    pub async fn definitions(&self) -> Result<Vec<DefinitionSummary>, Error> {
        let Some(bundled) = &self.bundled else {
            return Ok(Vec::new());
        };
        let _guard = bundled.snapshots.lock().await;
        let mut listed = Vec::new();
        for definition in bundled.definitions.iter() {
            listed.push(DefinitionSummary {
                reference: DefinitionReference::Bundled(definition.name().to_owned()).to_string(),
                digest: definition.digest().to_owned(),
                path: bundled.snapshots.write(definition).await?,
                workspace: definition.agent().spec.workspace,
            });
        }
        Ok(listed)
    }

    /// Deletes the ephemeral agent `id` once the client it belongs to is gone. Repeated deletion,
    /// and deleting one someone has deleted already, are safe.
    ///
    /// # Errors
    ///
    /// Returns an error when the deletion cannot be stored.
    pub async fn release(&self, id: AgentId) -> Result<(), Error> {
        let record = match self.store.get(id).await {
            Ok(record) => record,
            Err(Error::NotFound) => return Ok(()),
            Err(error) => return Err(error),
        };
        if record.lifetime != Lifetime::Ephemeral {
            return Ok(());
        }
        self.delete(&record.agent.metadata.name).await
    }

    /// Deletes the ephemeral agents an earlier run of agentd left, whose clients are gone, writes
    /// the copies of the bundled definitions this build carries, and removes the copies nothing
    /// uses. agentd calls it when it starts, before any client can ask for an agent.
    ///
    /// # Errors
    ///
    /// Returns an error when the agents cannot be read, a deletion cannot be stored, or a copy
    /// cannot be written or removed.
    pub async fn remove_leftovers(&self) -> Result<(), Error> {
        for record in self.store.list().await? {
            if record.lifetime == Lifetime::Ephemeral && record.agent.metadata.deletion_timestamp.is_none() {
                tracing::info!(agent = %record.agent.metadata.name, "deleting an ephemeral agent whose client is gone");
                self.delete(&record.agent.metadata.name).await?;
            }
        }
        if let Some(bundled) = &self.bundled {
            self.definitions().await?;
            bundled.snapshots.remove_unused(self.store.as_ref()).await?;
        }
        Ok(())
    }

    /// Prepares the agent the definition `reference` names declares, from agentd's copy of it.
    async fn prepare_reference(&self, reference: &str) -> Result<Prepared, Error> {
        let reference = DefinitionReference::parse(reference)?;
        let bundled = self
            .bundled
            .as_ref()
            .ok_or_else(|| Error::Daemon("this agentd carries no agent definitions".into()))?;
        let definition = bundled.definitions.get(reference.name())?;
        let directory = bundled.snapshots.write(definition).await?;
        let mut agent = definition.agent();
        agent.clear_managed_fields();
        Ok(Prepared {
            agent,
            manifest_path: Some(directory.join(crate::manifest::MANIFEST_FILE)),
            source_directory: directory,
            env_file: None,
            optional_mount_sources: std::collections::BTreeMap::new(),
            origin: Origin::Reference {
                reference: reference.to_string(),
                digest: definition.digest().to_owned(),
            },
        })
    }

    /// Prepares the agent a manifest on this machine declares, as `apply` does. A manifest in
    /// agentd's copy of a definition it runs by reference is that definition.
    pub(super) async fn prepare_local(
        &self,
        source_directory: PathBuf,
        manifest_path: Option<PathBuf>,
        env_file: Option<PathBuf>,
        mut agent: Agent,
    ) -> Result<Prepared, Error> {
        service::validate_request_paths(&source_directory, manifest_path.as_deref(), env_file.as_deref())?;
        agent.clear_managed_fields();
        let optional_mount_sources = service::resolve_mount_sources(&mut agent, &source_directory).await?;
        agent.validate()?;
        let origin = self.origin_of(&source_directory).await;
        if let Origin::Reference { reference, .. } = &origin
            && env_file.is_some()
        {
            return Err(Error::Invalid(format!(
                "agent definition {reference:?} is agentd's and reads no environment file"
            )));
        }
        Ok(Prepared {
            agent,
            source_directory,
            manifest_path,
            env_file,
            optional_mount_sources,
            origin,
        })
    }

    /// Where the definition in `source_directory` comes from: one of agentd's copies of a
    /// definition this build carries is that definition, and anything else is local.
    async fn origin_of(&self, source_directory: &Path) -> Origin {
        let Some(bundled) = &self.bundled else {
            return Origin::Local;
        };
        let directory = canonical(source_directory).await;
        for definition in bundled.definitions.iter() {
            if canonical(&bundled.snapshots.path(definition.digest())).await == directory {
                return Origin::Reference {
                    reference: DefinitionReference::Bundled(definition.name().to_owned()).to_string(),
                    digest: definition.digest().to_owned(),
                };
            }
        }
        Origin::Local
    }

    /// Removes the copies of bundled definitions nothing uses, logging a failure, which agentd
    /// retries when it starts.
    pub(super) async fn remove_unused_definitions(&self) {
        if let Some(bundled) = &self.bundled
            && let Err(error) = bundled.snapshots.remove_unused(self.store.as_ref()).await
        {
            tracing::warn!(%error, "removing the bundled definitions no agent uses failed; agentd retries when it starts");
        }
    }

    /// The canonical form of the workspace `path`, or why agentd refuses to mount it: it must be a
    /// folder, and must not be or hold the person's home folder or agentd's home, whose secrets and
    /// state an agent must not reach, nor be inside agentd's home.
    pub(super) async fn validate_workspace(&self, path: &Path) -> Result<PathBuf, Error> {
        if !path.is_absolute() {
            return Err(Error::Invalid(format!(
                "workspace {} must be an absolute path",
                path.display()
            )));
        }
        let workspace = tokio::fs::canonicalize(path)
            .await
            .map_err(|error| Error::Invalid(format!("workspace {} cannot be resolved: {error}", path.display())))?;
        if !tokio::fs::metadata(&workspace).await?.is_dir() {
            return Err(Error::Invalid(format!(
                "workspace {} must be a folder",
                workspace.display()
            )));
        }
        let home = match self.bundled.as_ref() {
            Some(bundled) => Some(canonical(&bundled.home).await),
            None => None,
        };
        if let Some(home) = &home
            && workspace.starts_with(home)
        {
            return Err(Error::Invalid(format!(
                "workspace {} is in agentd's home {}, which holds its state",
                workspace.display(),
                home.display()
            )));
        }
        let protected = [
            crate::local::home::user_home_directory().map(|user| (user, "your home folder")),
            home.map(|home| (home, "agentd's home, which holds its state,")),
        ];
        for (folder, what) in protected.into_iter().flatten() {
            let folder = canonical(&folder).await;
            if folder.starts_with(&workspace) {
                return Err(Error::Invalid(format!(
                    "workspace {} is or holds {what} {}; name the folder of one project instead",
                    workspace.display(),
                    folder.display()
                )));
            }
        }
        Ok(workspace)
    }

    /// Brings `current` to the agent `prepared` declares when it can change in place, and says
    /// what it did.
    async fn resume(&self, current: &AgentRecord, prepared: &Prepared) -> Result<(AgentRecord, RunOutcome), Error> {
        if current.agent.metadata.deletion_timestamp.is_some() {
            return Err(Error::Invalid(format!(
                "agent {:?} is being deleted; run it again once it is gone",
                current.agent.metadata.name
            )));
        }
        let mut desired = prepared.agent.clone();
        desired.metadata.name.clone_from(&current.agent.metadata.name);
        desired.validate()?;
        let outdated = |reason: String| Ok((current.clone(), RunOutcome::Outdated { reason }));
        match service::validate_immutable_fields(current, &desired) {
            Ok(()) => {}
            Err(Error::Immutable(field)) => {
                return outdated(format!("the definition changes {field}, which the agent cannot change"));
            }
            Err(error) => return Err(error),
        }
        let updated = current.origin != prepared.origin;
        if updated && let Some(home) = &desired.spec.home {
            // A home is copied into the agent once, when it is made, so a new one cannot reach it.
            let (old, new) = (
                current.source_directory.join(&home.source),
                prepared.source_directory.join(&home.source),
            );
            if !same_tree(old, new).await? {
                return outdated(
                    "the definition changes the files of spec.home, which the agent got when it was made".into(),
                );
            }
        }
        let mut record = current.clone();
        record.source_directory.clone_from(&prepared.source_directory);
        record.manifest_path.clone_from(&prepared.manifest_path);
        if prepared.env_file.is_some() {
            record.env_file.clone_from(&prepared.env_file);
        }
        record.origin = prepared.origin.clone();
        record.agent = desired;
        if !updated && record.agent.spec == current.agent.spec && record.env_file == current.env_file {
            return Ok((current.clone(), RunOutcome::Resumed));
        }
        record.agent.metadata.generation = current.agent.metadata.generation + 1;
        record.agent.metadata.restart_timestamp = current.agent.metadata.restart_timestamp;
        record.agent.status = current.agent.status.clone();
        self.reject_exposed_secrets(&record).await?;
        self.store
            .put(record.clone(), current.agent.metadata.generation)
            .await?;
        let outcome = if updated {
            RunOutcome::Updated
        } else {
            RunOutcome::Resumed
        };
        Ok((record, outcome))
    }
}

/// The bind mounts that give `agent` its workspace as `spec.workspace` allows, in a folder of the
/// sandbox's working directory named as the workspace is. A folder the agent may change but the
/// workspace lacks is made, since the sandbox cannot make a mount point in a read-only mount.
///
/// # Errors
///
/// Returns [`Error::Invalid`] when the agent declares a workspace and has none, or has one it does
/// not declare.
pub(super) async fn workspace_mounts(agent: &Agent, workspace: Option<&Path>) -> Result<Vec<MountSpec>, Error> {
    let spec = match (&agent.spec.workspace, workspace) {
        (None, None) => return Ok(Vec::new()),
        (Some(spec), Some(_)) => spec,
        (None, Some(_)) => {
            return Err(Error::Invalid(format!(
                "agent {:?} takes no workspace, as its definition has no spec.workspace",
                agent.metadata.name
            )));
        }
        (Some(_), None) => {
            return Err(Error::Invalid(format!(
                "agent {:?} needs a workspace, the folder it works on, as its definition has spec.workspace",
                agent.metadata.name
            )));
        }
    };
    let workspace = workspace.unwrap_or_else(|| Path::new(""));
    let working = crate::sandbox::platform::working_directory(&agent.spec.sandbox.platform.os).ok_or_else(|| {
        Error::Invalid(format!(
            "agentd has no working directory on {:?} to mount a workspace in",
            agent.spec.sandbox.platform.os
        ))
    })?;
    let name = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Invalid(format!("workspace {} needs a UTF-8 folder name", workspace.display())))?;
    let target = |relative: &Path| {
        let mut target = format!("{working}/{name}");
        for part in relative.components() {
            target.push('/');
            target.push_str(&part.as_os_str().to_string_lossy());
        }
        SandboxPath::new(target)
    };
    let WorkspaceSpec { read_only, writable } = spec;
    let mut mounts = vec![MountSpec::Bind {
        source: workspace.to_path_buf(),
        target: target(Path::new("")),
        read_only: *read_only,
        optional: false,
    }];
    for folder in writable {
        let source = workspace.join(folder);
        tokio::fs::create_dir_all(&source).await?;
        mounts.push(MountSpec::Bind {
            source: canonical(&source).await,
            target: target(folder),
            read_only: false,
            optional: false,
        });
    }
    Ok(mounts)
}

/// The name of a new agent of the definition named `definition`: its name, followed by the
/// workspace folder's, and a number when an agent has that name already.
fn available_name(definition: &str, workspace: Option<&Path>, records: &[AgentRecord]) -> Result<String, Error> {
    const MAX: usize = 63;
    let folder = workspace
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(name_part)
        .filter(|part| !part.is_empty());
    let base = folder.map_or_else(|| definition.to_owned(), |folder| format!("{definition}-{folder}"));
    let taken = |name: &str| records.iter().any(|record| record.agent.metadata.name == name);
    (1..=records.len() + 1)
        .map(|number| {
            let suffix = if number == 1 {
                String::new()
            } else {
                format!("-{number}")
            };
            let mut name = base.chars().take(MAX - suffix.len()).collect::<String>();
            name.truncate(name.trim_end_matches('-').len());
            name + &suffix
        })
        .find(|name| !taken(name))
        .ok_or_else(|| Error::Invalid(format!("no name is free for a new agent of {definition:?}")))
}

/// `text` with what an agent name cannot hold replaced by `-`.
fn name_part(text: &str) -> String {
    let mut part = String::new();
    for character in text.chars().map(|character| character.to_ascii_lowercase()) {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            part.push(character);
        } else if !part.is_empty() && !part.ends_with('-') {
            part.push('-');
        }
    }
    part.trim_end_matches('-').to_owned()
}

async fn canonical(path: &Path) -> PathBuf {
    tokio::fs::canonicalize(path)
        .await
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Whether the folders `old` and `new` hold the same files, with the same contents and
/// executable bits. A folder that does not exist holds none.
async fn same_tree(old: PathBuf, new: PathBuf) -> Result<bool, Error> {
    tokio::task::spawn_blocking(move || Ok::<_, std::io::Error>(read_tree(&old)? == read_tree(&new)?))
        .await
        .map_err(|error| Error::Daemon(format!("comparing agent definitions failed: {error}")))?
        .map_err(Error::from)
}

fn read_tree(root: &Path) -> std::io::Result<std::collections::BTreeMap<PathBuf, (bool, Vec<u8>)>> {
    let mut files = std::collections::BTreeMap::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let entries = match std::fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                folders.push(entry.path());
            } else {
                let relative = entry.path().strip_prefix(root).unwrap_or(&entry.path()).to_path_buf();
                files.insert(relative, (executable(&metadata), std::fs::read(entry.path())?));
            }
        }
    }
    Ok(files)
}

#[cfg(unix)]
fn executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_the_definitions_and_the_workspace_folders() {
        assert_eq!(
            available_name("builder", Some(Path::new("/src/My_Repo.git")), &[]).ok(),
            Some("builder-my-repo-git".into())
        );
        assert_eq!(available_name("sign-in", None, &[]).ok(), Some("sign-in".into()));
        assert_eq!(
            available_name("builder", Some(Path::new("/src/___")), &[]).ok(),
            Some("builder".into())
        );
        let long = "x".repeat(80);
        let name = available_name("builder", Some(Path::new(&format!("/src/{long}"))), &[]).expect("a name");
        assert_eq!(name.len(), 63);
    }
}
