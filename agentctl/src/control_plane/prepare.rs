//! Preparing an agent from a definition for the store, which `apply` and `run` share: reading the
//! definition, giving its bind mounts their folders, and storing a new agent.

use std::path::{Path, PathBuf};

use ::sandbox::SandboxPath;

use crate::{Agent, AgentId, DefinitionReference, Error, Lifetime, Origin};

use super::{AgentRecord, BindSource, ControlPlane, service};

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
}

impl ControlPlane {
    /// Stores a new agent made from `prepared`, whose bind mounts have their folders, kept for
    /// `lifetime`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Conflict`] when another agent took its name first, and an error when it
    /// would expose a secret or cannot be stored.
    pub(super) async fn create(&self, prepared: &Prepared, lifetime: Lifetime) -> Result<AgentRecord, Error> {
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
        self.store.put(record.clone(), 0).await?;
        Ok(record)
    }

    /// Prepares the agent the definition `reference` names declares, from agentd's copy of it.
    pub(super) async fn prepare_reference(&self, reference: &str) -> Result<Prepared, Error> {
        let reference = DefinitionReference::parse(reference)?;
        let bundled = self
            .bundled
            .as_ref()
            .ok_or_else(|| Error::Daemon("this agentd carries no agent definitions".into()))?;
        let definition = bundled.definitions.get(reference.name())?;
        let directory = bundled.folder(definition).await?;
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
        if let Origin::Reference { reference } = &origin {
            if env_file.is_some() {
                return Err(Error::Invalid(format!(
                    "agent definition {reference:?} is agentd's and reads no environment file"
                )));
            }
            // The copy's manifest names its image by `main`'s tag, and a release runs its own tag
            // of it, so it gets the image running the definition gets.
            if let Some(definition) = self.bundled.as_ref().and_then(|bundled| {
                let reference = DefinitionReference::parse(reference).ok()?;
                bundled.definitions.get(reference.name()).ok()
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
        // Any copy agentd keeps, including one of a definition this build no longer carries,
        // which it keeps while an agent made from it remains.
        let directory = canonical(source_directory).await;
        let name = directory.file_name().and_then(|name| name.to_str());
        match name {
            Some(name) if directory.parent() == Some(canonical(bundled.folders.root()).await.as_path()) => {
                Origin::Reference {
                    reference: DefinitionReference::Bundled(name.to_owned()).to_string(),
                }
            }
            _ => Origin::Local,
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

async fn canonical(path: &Path) -> PathBuf {
    tokio::fs::canonicalize(path)
        .await
        .unwrap_or_else(|_| path.to_path_buf())
}
