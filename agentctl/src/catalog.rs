//! The standard agents `agentctl setup` offers people to start from.
//!
//! Each archetype is a directory in `agents/standard/` with an `agent.yaml` on a published image,
//! so the host needs no Docker, and `agents/standard/catalog.yaml` describes them for the setup
//! wizard and for the KI-hub's directory. The build embeds the directory, so the catalog always
//! matches the manifest schema of the `agentctl` that offers it. `AGENTCTL_CATALOG_DIR` points at
//! a checkout's `agents/standard` instead, to try a change without a rebuild.
//!
//! The `Dockerfile` beside an archetype's manifest is what its image is built from. Setup builds
//! it here when the image cannot be pulled, such as before it is published, from the files beside
//! the `Dockerfile`; those in folders below it are left out.

use std::path::{Path, PathBuf};

use sandbox::{ByteQuantity, CpuQuantity, SandboxName};
use serde::{Deserialize, Serialize};

use crate::{Error, Harness, embedded, manifest::MANIFEST_FILE};

/// The variable that replaces the embedded catalog with a directory.
pub const CATALOG_DIRECTORY_VARIABLE: &str = "AGENTCTL_CATALOG_DIR";

/// The file describing the archetypes, at the root of the catalog.
pub const CATALOG_FILE: &str = "catalog.yaml";

/// The catalog's folder below `agents/`, as the build embeds it.
const EMBEDDED_FOLDER: &str = "standard";

/// One standard agent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Archetype {
    /// The archetype's directory below the catalog, such as `dotnet-backend-developer`.
    pub id: String,
    /// What people choose it by.
    pub title: String,
    /// What it is for and what it has, in a sentence or two.
    pub description: String,
    /// The agent name offered first, before one already taken is numbered.
    pub default_name: String,
    /// The smallest size it works at.
    pub minimum: Size,
    /// The size it is meant for, offered as the medium choice.
    pub recommended: Size,
    /// The harnesses its manifest declares, the default first.
    #[serde(default)]
    pub harnesses: Vec<Harness>,
    /// Words the directory can filter on.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Kept out of the choices, such as an archetype that is not ready.
    #[serde(default)]
    pub hidden: bool,
}

/// A sandbox size: processors, memory and root filesystem capacity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Size {
    /// Processors, as a quantity such as `"4"` or `"500m"`.
    pub cpu: CpuQuantity,
    /// Memory, such as `"8Gi"`.
    pub memory: ByteQuantity,
    /// Root filesystem capacity, such as `"64Gi"`.
    pub disk: ByteQuantity,
}

/// The standard agents, read from the embedded copy or from a directory.
#[derive(Debug)]
pub struct Catalog {
    archetypes: Vec<Archetype>,
    source: Source,
}

#[derive(Debug)]
enum Source {
    Embedded,
    Directory(PathBuf),
}

impl Catalog {
    /// The catalog from `AGENTCTL_CATALOG_DIR` when it is set, else the embedded one.
    ///
    /// # Errors
    ///
    /// Returns an error when the catalog file cannot be read or describes an archetype wrongly.
    pub fn load() -> Result<Self, Error> {
        std::env::var_os(CATALOG_DIRECTORY_VARIABLE)
            .filter(|value| !value.is_empty())
            .map_or_else(Self::embedded, |directory| {
                Self::from_directory(PathBuf::from(directory))
            })
    }

    /// The catalog this build embeds; empty when the build had none.
    ///
    /// # Errors
    ///
    /// Returns an error when the embedded catalog file describes an archetype wrongly.
    pub fn embedded() -> Result<Self, Error> {
        let archetypes = match embedded::file(&format!("{EMBEDDED_FOLDER}/{CATALOG_FILE}")) {
            Some(contents) => parse(contents, "the embedded catalog")?,
            None => Vec::new(),
        };
        Ok(Self {
            archetypes,
            source: Source::Embedded,
        })
    }

    /// The catalog in `directory`, a checkout's `agents/standard`.
    ///
    /// # Errors
    ///
    /// Returns an error when the catalog file cannot be read or describes an archetype wrongly.
    pub fn from_directory(directory: PathBuf) -> Result<Self, Error> {
        let path = directory.join(CATALOG_FILE);
        let contents = std::fs::read(&path)
            .map_err(|error| Error::Invalid(format!("cannot read the catalog {}: {error}", path.display())))?;
        let archetypes = parse(&contents, &path.display().to_string())?;
        Ok(Self {
            archetypes,
            source: Source::Directory(directory),
        })
    }

    /// Every archetype, hidden ones included, in catalog order.
    #[must_use]
    pub fn archetypes(&self) -> &[Archetype] {
        &self.archetypes
    }

    /// The archetypes people choose from.
    pub fn visible(&self) -> impl Iterator<Item = &Archetype> {
        self.archetypes.iter().filter(|archetype| !archetype.hidden)
    }

    /// The archetype with this id, hidden or not.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Archetype> {
        self.archetypes.iter().find(|archetype| archetype.id == id)
    }

    /// Writes the archetype's files into `destination` and returns the path of its manifest there.
    /// The files keep their places relative to the catalog, so the manifest's relative paths hold.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown archetype, one without a manifest, or a file that cannot be
    /// written.
    pub fn extract(&self, id: &str, destination: &Path) -> Result<PathBuf, Error> {
        let archetype = self
            .get(id)
            .ok_or_else(|| Error::Invalid(format!("the catalog has no archetype {id:?}")))?;
        match &self.source {
            Source::Embedded => embedded::extract(
                &format!("{EMBEDDED_FOLDER}/{}", archetype.id),
                &destination.join(&archetype.id),
            )?,
            Source::Directory(directory) => {
                embedded::copy_directory(&directory.join(&archetype.id), &destination.join(&archetype.id))?;
            }
        }
        let manifest = destination.join(&archetype.id).join(MANIFEST_FILE);
        if !manifest.is_file() {
            return Err(Error::Invalid(format!("archetype {id:?} has no {MANIFEST_FILE}")));
        }
        Ok(manifest)
    }
}

fn parse(contents: &[u8], origin: &str) -> Result<Vec<Archetype>, Error> {
    let archetypes: Vec<Archetype> =
        serde_yaml_ng::from_slice(contents).map_err(|error| Error::Invalid(format!("{origin}: {error}")))?;
    for (index, archetype) in archetypes.iter().enumerate() {
        validate(archetype).map_err(|reason| Error::Invalid(format!("{origin}: archetype {index}: {reason}")))?;
    }
    for (index, archetype) in archetypes.iter().enumerate() {
        if archetypes[..index].iter().any(|earlier| earlier.id == archetype.id) {
            return Err(Error::Invalid(format!(
                "{origin}: archetype {:?} appears twice",
                archetype.id
            )));
        }
    }
    Ok(archetypes)
}

fn validate(archetype: &Archetype) -> Result<(), String> {
    let id_is_a_directory = !archetype.id.is_empty()
        && archetype
            .id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !id_is_a_directory {
        return Err(format!("id {:?} must be lowercase letters, digits and -", archetype.id));
    }
    SandboxName::new(archetype.default_name.clone())
        .map_err(|error| format!("defaultName {:?}: {error}", archetype.default_name))?;
    let (minimum, recommended) = (archetype.minimum, archetype.recommended);
    if recommended.cpu.millicpus() < minimum.cpu.millicpus()
        || recommended.memory.bytes() < minimum.memory.bytes()
        || recommended.disk.bytes() < minimum.disk.bytes()
    {
        return Err(format!("{:?}: recommended is below minimum", archetype.id));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse;

    const CATALOG: &str = r#"
- id: dotnet-backend-developer
  title: ASP.NET Core backend
  description: Backend work with containers.
  defaultName: dotnet-backend
  minimum: {cpu: "2", memory: 6Gi, disk: 40Gi}
  recommended: {cpu: "4", memory: 12Gi, disk: 80Gi}
  harnesses: [pi]
  tags: [dotnet]
- id: agentbuilder
  title: Agent builder
  description: Coming.
  defaultName: agentbuilder
  minimum: {cpu: "2", memory: 4Gi, disk: 32Gi}
  recommended: {cpu: "2", memory: 4Gi, disk: 32Gi}
  hidden: true
"#;

    #[test]
    fn a_catalog_lists_its_archetypes_in_order() {
        let archetypes = parse(CATALOG.as_bytes(), "test").ok().unwrap_or_default();
        let ids: Vec<_> = archetypes.iter().map(|archetype| archetype.id.as_str()).collect();
        assert_eq!(ids, ["dotnet-backend-developer", "agentbuilder"]);
        assert!(archetypes[1].hidden);
        assert_eq!(archetypes[0].recommended.memory.to_string(), "12Gi");
    }

    #[test]
    fn a_catalog_refuses_bad_names_sizes_and_duplicates() {
        let bad_name = CATALOG.replace("defaultName: dotnet-backend", "defaultName: Dotnet_Backend");
        assert!(parse(bad_name.as_bytes(), "test").is_err());
        let small = CATALOG.replace("recommended: {cpu: \"4\"", "recommended: {cpu: \"1\"");
        assert!(parse(small.as_bytes(), "test").is_err());
        let twice = CATALOG.replace("id: agentbuilder", "id: dotnet-backend-developer");
        assert!(parse(twice.as_bytes(), "test").is_err());
        let unknown = CATALOG.replace("tags: [dotnet]", "labels: [dotnet]");
        assert!(parse(unknown.as_bytes(), "test").is_err());
    }
}
