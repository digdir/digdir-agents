//! The agent definitions agentd carries in its build, and its copies of them on disk.
//!
//! They come with the exact build because they are tied to its code, such as the sign-in recipes
//! or the schema a definition is written against. Otherwise they are ordinary definitions: agentd
//! writes each to a folder of its own, and an agent is applied or run from that folder as from
//! any other. The [`INTERNAL`] ones are the exception: only agentctl's own commands run them.

mod folders;

use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

pub use folders::Folders;

use crate::{Agent, Error, MountSpec, manifest::MANIFEST_FILE};

/// The images an agentd release publishes, by the tag `main` publishes them as. A release runs
/// its bundled definitions on the image it tagged for itself, `<main tag>-<version>`, so that a
/// definition always runs on the image built with it.
const RELEASED_IMAGES: &[&str] = &[
    "ghcr.io/digdir/agentctl-base:ubuntu-26.04",
    "ghcr.io/digdir/agentctl-base:ubuntu-26.04-desktop",
    "ghcr.io/digdir/agentctl-agent-builder:main",
];

/// The agent a harness login signs in in when this machine does not have the harness, which agentd
/// runs for that login alone.
pub const SIGN_IN: &str = "sign-in";

/// The definitions only agentctl's own commands run, each through a request of its own. They are
/// part of a command, and no use on their own, so agentd neither lists them nor runs them by
/// reference.
const INTERNAL: &[&str] = &[SIGN_IN];

/// One file of a definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionFile {
    /// The file's contents.
    pub contents: std::borrow::Cow<'static, [u8]>,
    /// Whether the file is executable.
    pub executable: bool,
}

/// A definition agentd carries, with its manifest decoded and checked.
#[derive(Clone, Debug)]
pub struct Definition {
    name: String,
    files: BTreeMap<String, DefinitionFile>,
    agent: Agent,
    internal: bool,
}

impl Definition {
    /// Decodes the definition named `name` from its files, by their `/`-separated paths in its
    /// folder, and checks that it names nothing outside them.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] when it has no `agent.yaml`, its manifest is invalid, or it names
    /// a host path, an environment file or a file it does not hold.
    pub fn new(name: impl Into<String>, files: BTreeMap<String, DefinitionFile>) -> Result<Self, Error> {
        let name = name.into();
        let invalid = |detail: String| Error::Invalid(format!("bundled definition {name:?}: {detail}"));
        // Its agents are named after it.
        ::sandbox::SandboxName::new(name.clone()).map_err(|error| invalid(format!("its name {error}")))?;
        let manifest = files
            .get(MANIFEST_FILE)
            .ok_or_else(|| invalid(format!("it has no {MANIFEST_FILE}")))?;
        let agent = crate::manifest::decode(&manifest.contents).map_err(|error| invalid(error.to_string()))?;
        check_contained(&agent, &files).map_err(invalid)?;
        Ok(Self {
            name,
            files,
            agent,
            internal: false,
        })
    }

    /// This definition as one only agentctl's own commands run, as [`Definitions::bundled`] makes
    /// those [`INTERNAL`] names.
    #[must_use]
    pub const fn into_internal(mut self) -> Self {
        self.internal = true;
        self
    }

    /// The definition's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its files, by their `/`-separated paths in its folder.
    #[must_use]
    pub const fn files(&self) -> &BTreeMap<String, DefinitionFile> {
        &self.files
    }

    /// The agent it defines, running on the image released with this build.
    #[must_use]
    pub fn agent(&self) -> Agent {
        let mut agent = self.agent.clone();
        if let ::sandbox::image::ImageSource::Reference { reference } = &mut agent.spec.sandbox.image {
            *reference = released_image(reference, crate::release_version());
        }
        agent
    }
}

/// The definitions agentd can run.
#[derive(Clone, Debug, Default)]
pub struct Definitions(BTreeMap<String, Definition>);

impl Definitions {
    /// The definitions this build carries.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] when one of them is invalid, which the tests catch before a build
    /// is released.
    pub fn bundled() -> Result<Self, Error> {
        let definitions = EMBEDDED
            .iter()
            .map(|embedded| {
                let files = embedded
                    .files
                    .iter()
                    .map(|file| {
                        (
                            file.path.to_owned(),
                            DefinitionFile {
                                contents: std::borrow::Cow::Borrowed(file.contents),
                                executable: file.executable,
                            },
                        )
                    })
                    .collect();
                let definition = Definition::new(embedded.name, files)?;
                Ok::<_, Error>(if INTERNAL.contains(&embedded.name) {
                    definition.into_internal()
                } else {
                    definition
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::new(definitions))
    }

    /// The definitions given, for tests and for [`Self::bundled`].
    #[must_use]
    pub fn new(definitions: impl IntoIterator<Item = Definition>) -> Self {
        Self(
            definitions
                .into_iter()
                .map(|definition| (definition.name.clone(), definition))
                .collect(),
        )
    }

    /// Every definition, by name, the internal ones too.
    pub fn iter(&self) -> impl Iterator<Item = &Definition> {
        self.0.values()
    }

    /// The definitions a client can list, apply and run by reference: all but the internal ones.
    pub fn listed(&self) -> impl Iterator<Item = &Definition> {
        self.iter().filter(|definition| !definition.internal)
    }

    /// The definition named `name` that a client runs by reference, which an internal one is not.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] when there is none.
    pub fn get(&self, name: &str) -> Result<&Definition, Error> {
        self.0
            .get(name)
            .filter(|definition| !definition.internal)
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "agentd carries no agent definition named {name:?}; is agentctl the same build as agentd?"
                ))
            })
    }

    /// The internal definition named `name`, for the request of agentctl's own that runs it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] when there is none.
    pub fn internal(&self, name: &str) -> Result<&Definition, Error> {
        self.0
            .get(name)
            .filter(|definition| definition.internal)
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "agentd carries no internal agent definition named {name:?}; is agentctl the same build as agentd?"
                ))
            })
    }
}

struct EmbeddedDefinition {
    name: &'static str,
    files: &'static [EmbeddedFile],
}

struct EmbeddedFile {
    path: &'static str,
    contents: &'static [u8],
    executable: bool,
}

/// Every folder of `bundled/definitions/`, as `bundled/build.rs` embeds it.
const EMBEDDED: &[EmbeddedDefinition] = include!(concat!(env!("OUT_DIR"), "/bundled_definitions.rs"));

/// `reference` as the release `version` runs it: an image the release publishes under its own
/// tag, and anything else unchanged. The release's tag is `main`'s tag followed by the version, or
/// the version alone for an image `main` tags `main`. Builds that are not releases, which have no
/// tag of their own, run `main`'s.
fn released_image(reference: &str, version: Option<&str>) -> String {
    match version {
        Some(version) if RELEASED_IMAGES.contains(&reference) && crate::release::Release::parse(version).is_some() => {
            reference.strip_suffix(":main").map_or_else(
                || format!("{reference}-{version}"),
                |repository| format!("{repository}:{version}"),
            )
        }
        _ => reference.to_owned(),
    }
}

/// Checks that `agent` reads only the files the definition holds: an agent of a bundled definition
/// reads agentd's copy of them, never the host's files, and binds only the folders it is given.
fn check_contained(agent: &Agent, files: &BTreeMap<String, DefinitionFile>) -> Result<(), String> {
    let spec = &agent.spec;
    if let ::sandbox::image::ImageSource::Build { .. } = spec.sandbox.image {
        return Err("spec.sandbox.image must be a reference, since a build context is a host folder".into());
    }
    if let Some(index) = spec
        .sandbox
        .mounts
        .iter()
        .position(|mount| matches!(mount, MountSpec::Bind { source: Some(_), .. }))
    {
        return Err(format!(
            "spec.sandbox.mounts[{index}] names a source, a folder on a host the definition does not know; \
             leave it out, and whoever runs the definition gives the folder"
        ));
    }
    let from_file = spec.environment_file_variables();
    if !from_file.is_empty() {
        return Err(format!(
            "it reads {} from an environment file, which a bundled definition has none of; give a plain value \
             with `value`, and name a secret stored on the machine with `name`",
            from_file.join(", ")
        ));
    }
    let holds_file = |path: &str| files.contains_key(path);
    let holds_folder = |path: &str| {
        files
            .keys()
            .any(|file| file.strip_prefix(path).is_some_and(|rest| rest.starts_with('/')))
    };
    let mut paths = Vec::new();
    if let Some(home) = &spec.home {
        paths.push(("spec.home.source".to_owned(), &home.source, false, true));
    }
    for (index, instructions) in spec.instructions.iter().enumerate() {
        paths.push((
            format!("spec.instructions[{index}].source"),
            &instructions.source,
            instructions.optional,
            false,
        ));
    }
    for (index, skill) in spec.skills.iter().enumerate() {
        paths.push((
            format!("spec.skills[{index}].source"),
            &skill.source,
            skill.optional,
            true,
        ));
    }
    for (field, path, optional, folder) in paths {
        let relative = relative_path(path).map_err(|detail| format!("{field} {detail}"))?;
        let present = if folder {
            holds_folder(&relative)
        } else {
            holds_file(&relative)
        };
        if !present && !optional {
            return Err(format!("{field} names {relative}, which the definition does not hold"));
        }
    }
    Ok(())
}

/// `path` as a `/`-separated path inside a folder, or why it is not one.
fn relative_path(path: &Path) -> Result<String, String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| format!("{} is not UTF-8", path.display()))?,
            ),
            Component::CurDir => {}
            _ => {
                return Err(format!(
                    "{} must be a path inside the definition's folder",
                    path.display()
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err(format!(
            "{} must name something inside the definition's folder",
            path.display()
        ));
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_definition_is_valid() {
        Definitions::bundled().expect("the bundled definitions");
    }

    /// A name in [`INTERNAL`] that no folder has, as after a rename, would leave the definition
    /// listed and run by reference.
    #[test]
    fn every_internal_definition_is_carried_and_runs_only_through_its_own_request() {
        let definitions = Definitions::bundled().expect("the bundled definitions");
        for name in INTERNAL {
            definitions
                .internal(name)
                .expect("an internal definition this build carries");
            definitions
                .get(name)
                .expect_err("an internal definition does not run by reference");
            assert!(definitions.listed().all(|definition| definition.name() != *name));
        }
    }

    #[test]
    fn the_images_a_release_runs_its_definitions_on_are_its_published_ones() {
        for image in super::RELEASED_IMAGES {
            assert!(crate::release::published_tag(image).is_some(), "{image}");
        }
    }

    #[test]
    fn only_a_release_runs_the_images_it_tagged() {
        let base = "ghcr.io/digdir/agentctl-base:ubuntu-26.04";
        assert_eq!(
            released_image(base, Some("v0.1.0-preview.13")),
            format!("{base}-v0.1.0-preview.13")
        );
        assert_eq!(released_image(base, Some("v1.2.3")), format!("{base}-v1.2.3"));
        assert_eq!(
            released_image("ghcr.io/digdir/agentctl-agent-builder:main", Some("v1.2.3")),
            "ghcr.io/digdir/agentctl-agent-builder:v1.2.3",
            "an image main tags `main` is tagged by the version alone"
        );
        for build in [
            None,
            Some("v0.1.0-preview.13.main.gabc1234"),
            Some("v0.1.1-main.gabc1234"),
            Some("v0.0.1-dev.20261108"),
        ] {
            assert_eq!(released_image(base, build), base, "{build:?}");
        }
        assert_eq!(
            released_image("ghcr.io/example/other:1", Some("v1.2.3")),
            "ghcr.io/example/other:1",
            "only the images a release publishes have its tag"
        );
    }
}
