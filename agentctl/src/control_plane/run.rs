//! Running an agent from a definition: one agentd resolves from a reference, or one on disk. Running
//! makes the agent in one request, mounts the folders it is given, and can tie its life to the
//! client that asked for it.

use std::path::{Path, PathBuf};

use ::sandbox::SandboxPath;

use crate::{
    Agent, AgentId, DefinitionReference, Error, Lifetime, Origin,
    bundled::{Definitions, Snapshots},
    definitions::DefinitionSummary,
};

use super::{AgentRecord, ControlPlane, service};

/// A request to run an agent from a definition.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunRequest {
    /// The definition to run.
    pub definition: RunDefinition,
    /// Host folders for the definition's bind mounts, in place of the sources it names or for
    /// those it names none. An agent kept until deleted is the only one of its definition on these
    /// folders unless it is named.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub binds: Vec<BindSource>,
    /// How long agentd keeps the agent; until it is deleted when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifetime: Option<Lifetime>,
    /// The agent's name. agentd chooses one from the definition and the folder it is given when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A host folder given for a bind mount when an agent is applied or run, as with
/// `--mount SOURCE[:TARGET]`.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BindSource {
    /// The absolute host folder.
    pub source: PathBuf,
    /// The target of the bind mount it is for. Without one, it is for the one bind mount the
    /// manifest names no source for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<SandboxPath>,
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
        /// Absolute path of the manifest, which with the folders it is given identifies the agent.
        manifest_path: PathBuf,
        /// Absolute path of the file supplying declared manifest values, when not `.env` beside it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env_file: Option<PathBuf>,
        /// The agent the manifest declares.
        agent: Box<Agent>,
    },
}

/// The agent a [`RunRequest`] made.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Ran {
    /// The agent as stored, which runs once the reconciler has started it.
    pub agent: Agent,
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

/// An agent as a definition declares it, ready to store once it has a name and its bind mounts
/// their sources.
#[derive(Clone)]
pub(super) struct Prepared {
    pub(super) agent: Agent,
    pub(super) source_directory: PathBuf,
    pub(super) manifest_path: Option<PathBuf>,
    pub(super) env_file: Option<PathBuf>,
    pub(super) optional_mount_sources: std::collections::BTreeMap<SandboxPath, PathBuf>,
    pub(super) origin: Origin,
    /// The bind mount sources the request gave, by target, other than those the manifest names.
    pub(super) bindings: std::collections::BTreeMap<SandboxPath, PathBuf>,
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

    /// Resolves the bind mounts' sources, with the `given` ones, which are canonical, in place of
    /// the manifest's.
    pub(super) async fn bind(&mut self, given: &[BindSource]) -> Result<(), Error> {
        let (optional, bindings) =
            service::resolve_mount_sources(&mut self.agent, &self.source_directory, given).await?;
        self.optional_mount_sources = optional;
        self.bindings = bindings;
        self.agent.validate()
    }

    /// Whether `record` is an agent kept until deleted made from this definition on the same
    /// folders.
    fn made(&self, record: &AgentRecord) -> bool {
        record.lifetime == Lifetime::UntilDeleted
            && record.agent.metadata.deletion_timestamp.is_none()
            && record.bindings == self.bindings
            && self.same_definition(record)
            // Variants share a folder but are different definitions.
            && (!self.origin.is_local() || record.manifest_path == self.manifest_path)
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
    /// Makes an agent from a definition and starts it.
    ///
    /// Running never changes an agent that exists. Without a name, an agent kept until deleted is
    /// refused when one of the same definition already works on the same folders, which the
    /// error names so the client can attach to it, and agentd names the new one after the
    /// definition and the folder it is given. An ephemeral one belongs to the request alone, so
    /// each request makes its own, which agentd deletes once the client that asked for it is gone.
    ///
    /// # Errors
    ///
    /// Returns an error when the definition is invalid or unknown, a folder is refused, the name
    /// belongs to another agent, an agent of the definition already works on the folders, or the
    /// agent cannot be stored.
    pub async fn run(&self, request: RunRequest) -> Result<(Ran, AgentId), Error> {
        let lifetime = request.lifetime.unwrap_or_default();
        let binds = self.validate_bind_sources(&request.binds).await?;
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
        Box::pin(prepared.bind(&binds)).await?;
        prepared.agent.spec.set_run_state(crate::RunState::Running);

        let record = loop {
            let records = self.store.list().await?;
            if let Some(name) = &request.name {
                if records.iter().any(|record| record.agent.metadata.name == *name) {
                    return Err(Error::Invalid(format!("an agent named {name:?} already exists")));
                }
            } else if lifetime == Lifetime::UntilDeleted
                && let Some(running) = records.iter().find(|record| prepared.made(record))
            {
                return Err(already_running(running));
            }
            prepared.agent.metadata.name = match &request.name {
                Some(name) => name.clone(),
                None => available_name(
                    prepared.base_name(),
                    prepared.bindings.values().next().map(PathBuf::as_path),
                    &records,
                )?,
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
                bindings: prepared.bindings.clone(),
            };
            record.agent.metadata.generation = 1;
            self.reject_exposed_secrets(&record).await?;
            match self.store.put(record.clone(), 0).await {
                Ok(()) => break record,
                // Another request took the name first.
                Err(Error::Conflict) => {}
                Err(error) => return Err(error),
            }
        };
        drop(guard);

        self.notifier.notify(record.id);
        let id = record.id;
        Ok((
            Ran {
                agent: self.resource(record),
            },
            id,
        ))
    }

    /// The definitions a client can apply or run: those on disk for `directory`, as
    /// [`crate::definitions::manifests`] finds them, then the manifests agents kept until deleted
    /// were applied from, then the ones agentd carries, each in the folder agentd keeps its copy
    /// in, written if missing.
    ///
    /// # Errors
    ///
    /// Returns an error when `directory` is not absolute, the agents cannot be read, or a copy
    /// cannot be written.
    pub async fn definitions(&self, directory: Option<PathBuf>) -> Result<Vec<DefinitionSummary>, Error> {
        if let Some(directory) = &directory
            && !directory.is_absolute()
        {
            return Err(Error::Invalid(format!(
                "directory {} must be an absolute path",
                directory.display()
            )));
        }
        // An agent run by reference was made from agentd's copy of a version of the definition,
        // which the definitions agentd carries now stand for.
        let recorded = self
            .store
            .list()
            .await?
            .into_iter()
            .filter(|record| {
                record.origin.is_local()
                    && record.lifetime == Lifetime::UntilDeleted
                    && record.agent.metadata.deletion_timestamp.is_none()
            })
            .map(|record| {
                record
                    .manifest_path
                    .unwrap_or_else(|| record.source_directory.join(crate::manifest::MANIFEST_FILE))
            })
            .collect();
        let mut listed =
            tokio::task::spawn_blocking(move || crate::definitions::on_disk(directory.as_deref(), recorded))
                .await
                .map_err(|error| Error::Daemon(format!("listing the definitions on disk failed: {error}")))?;
        listed.extend(self.bundled_definitions().await?);
        Ok(listed)
    }

    /// The definitions agentd carries, each in the folder agentd keeps its copy in, written if
    /// missing.
    async fn bundled_definitions(&self) -> Result<Vec<DefinitionSummary>, Error> {
        let Some(bundled) = &self.bundled else {
            return Ok(Vec::new());
        };
        let _guard = bundled.snapshots.lock().await;
        let mut listed = Vec::new();
        for definition in bundled.definitions.iter() {
            let agent = definition.agent();
            listed.push(DefinitionSummary {
                path: bundled
                    .snapshots
                    .write(definition)
                    .await?
                    .join(crate::manifest::MANIFEST_FILE),
                aliases: Vec::new(),
                reference: Some(DefinitionReference::Bundled(definition.name().to_owned()).to_string()),
                digest: Some(definition.digest().to_owned()),
                name: Some(agent.metadata.name.clone()),
                error: None,
                slots: crate::definitions::unsourced_binds(&agent),
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
            self.bundled_definitions().await?;
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
            bindings: std::collections::BTreeMap::new(),
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
        agent.validate()?;
        let origin = self.origin_of(&source_directory).await;
        if let Origin::Reference { reference, digest } = &origin {
            if env_file.is_some() {
                return Err(Error::Invalid(format!(
                    "agent definition {reference:?} is agentd's and reads no environment file"
                )));
            }
            // The copy's manifest names its image by `main`'s tag, and a release runs its own tag
            // of it, so it gets the image running the definition gets.
            if let Some(definition) = self.bundled.as_ref().and_then(|bundled| {
                bundled
                    .definitions
                    .iter()
                    .find(|definition| definition.digest() == digest)
            }) {
                agent.spec.sandbox.image = definition.agent().spec.sandbox.image;
            }
        }
        Ok(Prepared {
            agent,
            source_directory,
            manifest_path,
            env_file,
            optional_mount_sources: std::collections::BTreeMap::new(),
            origin,
            bindings: std::collections::BTreeMap::new(),
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

    /// The `given` bind mount sources, each canonical, or why agentd refuses to mount one.
    pub(super) async fn validate_bind_sources(&self, given: &[BindSource]) -> Result<Vec<BindSource>, Error> {
        let mut validated = Vec::with_capacity(given.len());
        for bind in given {
            validated.push(BindSource {
                source: self.validate_bind_source(&bind.source).await?,
                target: bind.target.clone(),
            });
        }
        Ok(validated)
    }

    /// The canonical form of the folder `path`, given for a bind mount, or why agentd refuses to
    /// mount it: it must be a folder, and must not be or hold the person's home folder or agentd's
    /// home, whose secrets and state an agent must not reach, nor be inside agentd's home.
    async fn validate_bind_source(&self, path: &Path) -> Result<PathBuf, Error> {
        if !path.is_absolute() {
            return Err(Error::Invalid(format!(
                "folder {} must be an absolute path",
                path.display()
            )));
        }
        let folder = tokio::fs::canonicalize(path)
            .await
            .map_err(|error| Error::Invalid(format!("folder {} cannot be resolved: {error}", path.display())))?;
        if !tokio::fs::metadata(&folder).await?.is_dir() {
            return Err(Error::Invalid(format!("{} must be a folder", folder.display())));
        }
        let home = match self.bundled.as_ref() {
            Some(bundled) => Some(canonical(&bundled.home).await),
            None => None,
        };
        if let Some(home) = &home
            && folder.starts_with(home)
        {
            return Err(Error::Invalid(format!(
                "folder {} is in agentd's home {}, which holds its state",
                folder.display(),
                home.display()
            )));
        }
        let protected = [
            crate::local::home::user_home_directory().map(|user| (user, "your home folder")),
            home.map(|home| (home, "agentd's home, which holds its state,")),
        ];
        for (protected, what) in protected.into_iter().flatten() {
            let protected = canonical(&protected).await;
            if protected.starts_with(&folder) {
                return Err(Error::Invalid(format!(
                    "folder {} is or holds {what} {}; name the folder of one project instead",
                    folder.display(),
                    protected.display()
                )));
            }
        }
        Ok(folder)
    }
}

/// Why running a definition is refused when `running` is already an agent of it on the same
/// folders: the request can attach to it, or name another.
fn already_running(running: &AgentRecord) -> Error {
    let name = &running.agent.metadata.name;
    let definition = match &running.origin {
        Origin::Reference { reference, .. } => reference.clone(),
        Origin::Local => running
            .manifest_path
            .as_deref()
            .unwrap_or(&running.source_directory)
            .display()
            .to_string(),
    };
    let folders = running
        .bindings
        .values()
        .map(|folder| folder.display().to_string())
        .collect::<Vec<_>>();
    let on = if folders.is_empty() {
        String::new()
    } else {
        format!(" on {}", folders.join(", "))
    };
    Error::Invalid(format!(
        "agent {name:?} already runs {definition}{on}; attach to it with `agentctl attach session/main --agent {name}`, \
         or name another with --name"
    ))
}

/// The name of a new agent of the definition named `definition`: its name, followed by the name
/// of the folder it was given, and a number when an agent has that name already.
fn available_name(definition: &str, folder: Option<&Path>, records: &[AgentRecord]) -> Result<String, Error> {
    const MAX: usize = 63;
    let folder = folder
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_the_definitions_and_the_given_folders() {
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
