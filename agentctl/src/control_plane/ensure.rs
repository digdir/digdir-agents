//! Agents agentd runs from the definitions it carries, which clients find or make in one request.

use std::path::{Path, PathBuf};

use ::sandbox::SandboxPath;

use crate::{
    Agent, AgentId, Error, Lifetime, MountSpec, Origin,
    bundled::{Definitions, Snapshots, Workspace},
};

use super::{AgentRecord, ControlPlane, service};

/// A client's request for an agent of one of agentd's own definitions.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EnsureRequest {
    /// The bundled definition's name.
    pub definition: String,
    /// An absolute host folder to bind into the agent, for a definition that takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    /// How long agentd keeps the agent, when not as long as the definition says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifetime: Option<Lifetime>,
}

/// The agent an [`EnsureRequest`] found or made, and what agentd did to it.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Ensured {
    /// The agent as stored, which runs once the reconciler has started it.
    pub agent: Agent,
    /// What agentd did to it.
    pub outcome: EnsureOutcome,
}

/// What [`ControlPlane::ensure`] did to the agent it returns.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EnsureOutcome {
    /// It made the agent.
    Created,
    /// It found the agent, made from the definition as this build carries it, and started it if it
    /// was stopped.
    Resumed,
    /// It found the agent, made from an earlier build's definition, and changed it in place to this
    /// build's, starting it if it was stopped.
    Updated,
    /// It found the agent, made from an earlier build's definition, which changes something an
    /// agent cannot change in place. It left the agent as it was: deleting it and asking again
    /// makes it from this build's definition.
    Outdated {
        /// What cannot change in place.
        reason: String,
    },
}

/// The definitions [`ControlPlane::ensure`] runs agents from, and where it keeps their copies.
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

impl ControlPlane {
    /// Finds the agent of a bundled definition for a workspace, or makes it, and starts it.
    ///
    /// An agent kept until deleted is the one agent for its definition and workspace, so asking
    /// again resumes it. An ephemeral one belongs to the request alone, so each request makes its
    /// own. agentd chooses the agent's name, and never changes or replaces an agent it did not make
    /// from this definition.
    ///
    /// # Errors
    ///
    /// Returns an error when agentd carries no such definition, the workspace is refused, the
    /// agent is being deleted, or it cannot be stored.
    pub async fn ensure(&self, request: EnsureRequest) -> Result<(Ensured, AgentId), Error> {
        let bundled = self
            .bundled
            .as_ref()
            .ok_or_else(|| Error::Daemon("this agentd runs no bundled definitions".into()))?;
        let definition = bundled.definitions.get(&request.definition)?;
        let lifetime = request.lifetime.unwrap_or_else(|| definition.lifetime());
        let workspace = match (&request.workspace, definition.workspace()) {
            (Some(path), Some(_)) => Some(validate_workspace(path, &bundled.home).await?),
            (Some(_), None) => {
                return Err(Error::Invalid(format!(
                    "agent definition {:?} takes no workspace",
                    definition.name()
                )));
            }
            (None, Some(_)) => {
                return Err(Error::Invalid(format!(
                    "agent definition {:?} needs a workspace: the folder it works on",
                    definition.name()
                )));
            }
            (None, None) => None,
        };

        // Held until the agent using the copy is stored, so cleanup cannot remove it first. It also
        // has requests find or make their agents one at a time. Two that race anyway, with an
        // apply between them, cannot both make the agent, since the store takes a name once.
        let snapshot = bundled.snapshots.lock().await;
        let directory = bundled.snapshots.write(definition).await?;
        let mut desired = definition.agent();
        desired.clear_managed_fields();
        desired.spec.set_run_state(crate::RunState::Running);
        if let (Some(workspace), Some(access)) = (&workspace, definition.workspace()) {
            desired
                .spec
                .sandbox
                .mounts
                .extend(workspace_mounts(&desired, workspace, access).await?);
        }
        let origin = Origin::Bundled {
            definition: definition.name().to_owned(),
            digest: definition.digest().to_owned(),
        };

        let ensured = loop {
            let records = self.store.list().await?;
            let current = (lifetime == Lifetime::UntilDeleted)
                .then(|| {
                    records.iter().find(|record| {
                        record.lifetime == Lifetime::UntilDeleted
                            && record.workspace == workspace
                            && matches!(&record.origin, Origin::Bundled { definition: name, .. } if name == definition.name())
                    })
                })
                .flatten();
            let result = if let Some(current) = current {
                self.resume(current, desired.clone(), &directory, &origin).await
            } else {
                desired.metadata.name = available_name(definition.name(), workspace.as_deref(), &records)?;
                desired.validate()?;
                let mut record = AgentRecord {
                    id: AgentId::generate(),
                    source_directory: directory.clone(),
                    manifest_path: None,
                    env_file: None,
                    agent: desired.clone(),
                    optional_mount_sources: std::collections::BTreeMap::new(),
                    volume_revisions: std::collections::BTreeMap::new(),
                    origin: origin.clone(),
                    lifetime,
                    workspace: workspace.clone(),
                };
                record.agent.metadata.generation = 1;
                self.reject_exposed_secrets(&record).await?;
                self.store
                    .put(record.clone(), 0)
                    .await
                    .map(|()| (record, EnsureOutcome::Created))
            };
            match result {
                // Another request stored the agent or took its name first.
                Err(Error::Conflict) => {}
                result => break result?,
            }
        };
        drop(snapshot);

        let (record, outcome) = ensured;
        self.notifier.notify(record.id);
        if outcome == EnsureOutcome::Updated
            && let Err(error) = bundled.snapshots.remove_unused(self.store.as_ref()).await
        {
            tracing::warn!(%error, "removing the bundled definitions no agent uses failed; agentd retries when it starts");
        }
        let id = record.id;
        Ok((
            Ensured {
                agent: self.resource(record),
                outcome,
            },
            id,
        ))
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

    /// Deletes the ephemeral agents an earlier run of agentd left, whose clients are gone, and the
    /// copies of bundled definitions no agent uses. agentd calls it when it starts, before any
    /// client can ask for an agent.
    ///
    /// # Errors
    ///
    /// Returns an error when the agents cannot be read or a deletion cannot be stored.
    pub async fn remove_leftovers(&self) -> Result<(), Error> {
        for record in self.store.list().await? {
            if record.lifetime == Lifetime::Ephemeral && record.agent.metadata.deletion_timestamp.is_none() {
                tracing::info!(agent = %record.agent.metadata.name, "deleting an ephemeral agent whose client is gone");
                self.delete(&record.agent.metadata.name).await?;
            }
        }
        if let Some(bundled) = &self.bundled {
            bundled.snapshots.remove_unused(self.store.as_ref()).await?;
        }
        Ok(())
    }

    /// Brings `current` to `desired`, made from the definition copied to `directory`, when it can
    /// change in place, and says what it did.
    async fn resume(
        &self,
        current: &AgentRecord,
        mut desired: Agent,
        directory: &Path,
        origin: &Origin,
    ) -> Result<(AgentRecord, EnsureOutcome), Error> {
        if current.agent.metadata.deletion_timestamp.is_some() {
            return Err(Error::Invalid(format!(
                "agent {:?} is being deleted; ask again once it is gone",
                current.agent.metadata.name
            )));
        }
        desired.metadata.name.clone_from(&current.agent.metadata.name);
        desired.validate()?;
        let outdated = |reason: String| Ok((current.clone(), EnsureOutcome::Outdated { reason }));
        match service::validate_immutable_fields(current, &desired) {
            Ok(()) => {}
            Err(Error::Immutable(field)) => {
                return outdated(format!(
                    "this agentd's definition changes {field}, which the agent cannot change"
                ));
            }
            Err(error) => return Err(error),
        }
        let updated = current.origin != *origin;
        if updated && let Some(home) = &desired.spec.home {
            // A home is copied into the agent once, when it is made, so a new one cannot reach it.
            let (old, new) = (
                current.source_directory.join(&home.source),
                directory.join(&home.source),
            );
            if !same_tree(old, new).await? {
                return outdated(
                    "this agentd's definition changes the files of spec.home, which the agent got when it was made"
                        .into(),
                );
            }
        }
        if !updated && current.agent.spec == desired.spec {
            return Ok((current.clone(), EnsureOutcome::Resumed));
        }
        let mut record = current.clone();
        record.source_directory = directory.to_path_buf();
        record.origin = origin.clone();
        record.agent = desired;
        record.agent.metadata.generation = current.agent.metadata.generation + 1;
        record.agent.metadata.restart_timestamp = current.agent.metadata.restart_timestamp;
        record.agent.status = current.agent.status.clone();
        self.reject_exposed_secrets(&record).await?;
        self.store
            .put(record.clone(), current.agent.metadata.generation)
            .await?;
        let outcome = if updated {
            EnsureOutcome::Updated
        } else {
            EnsureOutcome::Resumed
        };
        Ok((record, outcome))
    }
}

/// The canonical form of the workspace `path`, or why agentd refuses to bind it: it must be a
/// folder, and must not be or hold the person's home folder or agentd's home, `home`, whose
/// secrets and state an agent must not reach, nor be inside agentd's home.
async fn validate_workspace(path: &Path, home: &Path) -> Result<PathBuf, Error> {
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
    let home = canonical(home).await;
    if workspace.starts_with(&home) {
        return Err(Error::Invalid(format!(
            "workspace {} is in agentd's home {}, which holds its state",
            workspace.display(),
            home.display()
        )));
    }
    let protected = [
        crate::local::home::user_home_directory().map(|user| (user, "your home folder")),
        Some((home, "agentd's home, which holds its state,")),
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

/// The bind mounts that give the agent the workspace as `access` allows, in a folder of the
/// sandbox's working directory named as the workspace is. A folder the agent may change but the
/// workspace lacks is made, so that the agent can write what it is for.
async fn workspace_mounts(agent: &Agent, workspace: &Path, access: &Workspace) -> Result<Vec<MountSpec>, Error> {
    let working = crate::sandbox::platform::working_directory(&agent.spec.sandbox.platform.os).ok_or_else(|| {
        Error::Invalid(format!(
            "agentd has no working directory on {:?} to bind a workspace in",
            agent.spec.sandbox.platform.os
        ))
    })?;
    let name = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Invalid(format!("workspace {} needs a UTF-8 folder name", workspace.display())))?;
    let target = |relative: &str| SandboxPath::new(format!("{working}/{name}{relative}"));
    let mut mounts = vec![MountSpec::Bind {
        source: workspace.to_path_buf(),
        target: target(""),
        read_only: matches!(access, Workspace::ReadOnly { .. }),
        optional: false,
    }];
    if let Workspace::ReadOnly { writable } = access {
        for folder in writable {
            let source = workspace.join(folder);
            tokio::fs::create_dir_all(&source).await?;
            mounts.push(MountSpec::Bind {
                source: canonical(&source).await,
                target: target(&format!("/{folder}")),
                read_only: false,
                optional: false,
            });
        }
    }
    Ok(mounts)
}

/// The name of a new agent of `definition`: the definition's, followed by the workspace folder's,
/// and a number when an agent has that name already.
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
        .map_err(|error| Error::Daemon(format!("comparing bundled definitions failed: {error}")))?
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
