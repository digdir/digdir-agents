//! The definitions agentd carries, and listing every definition a client can run.

use std::path::PathBuf;

use ::sandbox::blocking::{self, Work};

use crate::{
    DefinitionReference, Error, Lifetime,
    bundled::{Definition, Definitions, Folders},
    definitions::DefinitionSummary,
};

use super::ControlPlane;

/// The definitions agentd carries, and where it keeps their copies.
pub(super) struct Bundled {
    pub(super) definitions: Definitions,
    pub(super) folders: Folders,
    /// agentd's home, which no folder given for a bind mount may hold or be in.
    pub(super) home: PathBuf,
}

impl Bundled {
    pub(super) fn new(definitions: Definitions, home: PathBuf) -> Self {
        Self {
            definitions,
            folders: Folders::new(home.join("bundled")),
            home,
        }
    }

    /// The folder of agentd's copy of `definition`, written if missing.
    pub(super) async fn folder(&self, definition: &Definition) -> Result<PathBuf, Error> {
        self.folders.ensure(definition).await
    }
}

impl ControlPlane {
    /// The definitions a client can apply or run: those on disk for `directory`, as
    /// [`crate::definitions::manifests`] finds them, then the manifests agents kept until deleted
    /// were applied from, then the ones agentd carries, each in the folder agentd keeps its copy
    /// in, written if missing. Those on disk tell as much as `detail` asks for.
    ///
    /// # Errors
    ///
    /// Returns an error when `directory` is not absolute, the agents cannot be read, or a copy
    /// cannot be written.
    pub async fn definitions(
        &self,
        directory: Option<PathBuf>,
        detail: crate::definitions::Detail,
    ) -> Result<Vec<DefinitionSummary>, Error> {
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
        let mut listed = blocking::run(Work::Tree, "listing the definitions on disk", move || {
            crate::definitions::on_disk(directory.as_deref(), recorded, detail)
        })
        .await?;
        listed.extend(self.bundled_definitions().await?);
        Ok(listed)
    }

    /// The definitions agentd carries that a client can run, each in the folder agentd keeps its
    /// copy in, written if missing.
    async fn bundled_definitions(&self) -> Result<Vec<DefinitionSummary>, Error> {
        let Some(bundled) = &self.bundled else {
            return Ok(Vec::new());
        };
        let mut listed = Vec::new();
        for definition in bundled.definitions.listed() {
            let agent = definition.agent();
            listed.push(DefinitionSummary {
                path: bundled.folder(definition).await?.join(crate::manifest::MANIFEST_FILE),
                aliases: Vec::new(),
                reference: Some(DefinitionReference::Bundled(definition.name().to_owned()).to_string()),
                name: Some(agent.metadata.name.clone()),
                error: None,
                slots: crate::definitions::unsourced_binds(&agent),
                // agentd checks that the definitions it carries read nothing from one.
                environment_file_variables: Vec::new(),
                environment_file: None,
                platform_images: Vec::new(),
                ignored: false,
            });
        }
        Ok(listed)
    }

    /// Deletes the ephemeral agents an earlier run of agentd left, whose clients are gone, writes
    /// the copies of the bundled definitions this build carries over the ones there, and removes
    /// the copies of others no agent was made from. agentd calls it when it starts, before any
    /// client can ask for an agent.
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
            for definition in bundled.definitions.iter() {
                bundled.folders.write(definition).await?;
            }
            bundled
                .folders
                .remove_unused(&bundled.definitions, self.store.as_ref())
                .await?;
        }
        Ok(())
    }
}
