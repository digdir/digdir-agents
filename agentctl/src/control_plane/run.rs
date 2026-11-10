//! Running an agent from a definition: one agentd resolves from a reference, or one on disk. Running
//! makes the agent in one request, mounts the folders it is given, and can tie its life to the
//! client that asked for it.

use std::path::{Path, PathBuf};

use ::sandbox::SandboxPath;

use crate::{Agent, AgentId, Error, Lifetime, Origin};

use super::{AgentRecord, ControlPlane, prepare::Prepared};

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
    /// The files that may hold real keys, such as a checkout's `.env`, that the person agreed to let
    /// the agent read, as absolute host paths. agentd refuses the request when its mounts hold any
    /// other.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub allow_exposed_files: std::collections::BTreeSet<PathBuf>,
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

impl Prepared {
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

    /// The folder an agent is named after: the first one given, in the order the definition
    /// declares its mounts, so a definition decides which of its folders names its agents.
    fn naming_folder(&self) -> Option<&Path> {
        first_given_folder(&self.agent, &self.bindings)
    }
}

/// The first of `bindings` in the order `agent` declares its bind mounts.
fn first_given_folder<'a>(
    agent: &Agent,
    bindings: &'a std::collections::BTreeMap<SandboxPath, PathBuf>,
) -> Option<&'a Path> {
    agent.spec.sandbox.mounts.iter().find_map(|mount| match mount {
        crate::MountSpec::Bind { target, .. } => bindings.get(target).map(PathBuf::as_path),
        crate::MountSpec::Volume { .. } | crate::MountSpec::Tmpfs { .. } => None,
    })
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
        let binds = self.validate_bind_sources(&request.binds).await?;
        let prepared = match request.definition {
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
        let lifetime = request.lifetime.unwrap_or_default();
        Box::pin(self.run_prepared(
            prepared,
            &binds,
            lifetime,
            request.name.as_deref(),
            &request.allow_exposed_files,
        ))
        .await
    }

    /// Makes the agent a sign-in runs in when this machine does not have its program, from the
    /// internal definition named `definition`, such as `sign-in-github`, and
    /// starts it. It is ephemeral, so each request makes its own, which agentd deletes once the
    /// client that asked for it is gone.
    ///
    /// # Errors
    ///
    /// Returns an error when this agentd does not carry the definition or the agent cannot be
    /// stored.
    pub async fn run_sign_in(&self, definition: &str) -> Result<(Ran, AgentId), Error> {
        let prepared = self
            .prepare_bundled(|definitions| definitions.internal(definition))
            .await?;
        Box::pin(self.run_prepared(
            prepared,
            &[],
            Lifetime::Ephemeral,
            None,
            &std::collections::BTreeSet::new(),
        ))
        .await
    }

    /// Gives `prepared` its folders and a name, stores it and starts it, as [`Self::run`] says.
    async fn run_prepared(
        &self,
        mut prepared: Prepared,
        binds: &[BindSource],
        lifetime: Lifetime,
        name: Option<&str>,
        allow_exposed_files: &std::collections::BTreeSet<PathBuf>,
    ) -> Result<(Ran, AgentId), Error> {
        Box::pin(prepared.bind(binds)).await?;
        prepared.agent.spec.set_run_state(crate::RunState::Running);

        let record = loop {
            let records = self.store.list().await?;
            if let Some(name) = name {
                if records.iter().any(|record| record.agent.metadata.name == name) {
                    return Err(Error::Invalid(format!("an agent named {name:?} already exists")));
                }
            } else if lifetime == Lifetime::UntilDeleted
                && let Some(running) = records.iter().find(|record| prepared.made(record))
            {
                return Err(already_running(running));
            }
            prepared.agent.metadata.name = match name {
                Some(name) => name.to_owned(),
                None => available_name(prepared.base_name(), prepared.naming_folder(), &records)?,
            };
            prepared.agent.validate()?;
            // Another request that took the name first is found on the next pass: it may be an
            // agent of this definition on these folders, which this one is then refused for.
            match Box::pin(self.create(&prepared, lifetime, allow_exposed_files)).await {
                Ok(record) => break record,
                Err(Error::Conflict) => {}
                Err(error) => return Err(error),
            }
        };

        self.notifier.notify(record.id);
        let id = record.id;
        Ok((
            Ran {
                agent: self.resource(record),
            },
            id,
        ))
    }

    /// The files that may hold real keys in the folders `binds` give, as running an agent on them
    /// finds them, so that a client can ask the person about them, or say why they are refused,
    /// before it changes anything else.
    ///
    /// # Errors
    ///
    /// Returns an error when a folder is refused, as running would refuse it, or the agents or the
    /// folders cannot be read.
    pub async fn exposed_files(&self, binds: &[BindSource]) -> Result<Vec<crate::exposed::ExposedFile>, Error> {
        let folders = self
            .validate_bind_sources(binds)
            .await?
            .into_iter()
            .map(|bind| (bind.source, bind.target))
            .collect();
        let environment_files = super::service::environment_files(self.store.as_ref()).await?;
        crate::exposed::find(folders, environment_files).await
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_is_named_after_the_folder_its_definition_mounts_first() {
        // The agent builder mounts the checkout first, at a target that sorts after its other one.
        let mut agent = crate::bundled::Definitions::bundled()
            .expect("the bundled definitions")
            .get("agent-builder")
            .expect("the agent builder")
            .agent();
        let targets = agent
            .spec
            .sandbox
            .mounts
            .iter()
            .filter_map(|mount| match mount {
                crate::MountSpec::Bind { target, .. } => Some(target.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let [first, second] = targets.as_slice() else {
            panic!("the agent builder mounts two folders: {targets:?}");
        };
        assert!(second < first, "the targets sort in the other order");
        let bindings = [
            (first.clone(), PathBuf::from("/src/service")),
            (second.clone(), PathBuf::from("/home/person/agents")),
        ]
        .into_iter()
        .collect();
        assert_eq!(first_given_folder(&agent, &bindings), Some(Path::new("/src/service")));
        agent.spec.sandbox.mounts.reverse();
        assert_eq!(
            first_given_folder(&agent, &bindings),
            Some(Path::new("/home/person/agents"))
        );
    }

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
