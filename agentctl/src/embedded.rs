//! The agent definitions the build embeds from `agents/`, the standard agents and the agent
//! builder, so `agentctl setup` and `agentctl new` run them offline. See `build.rs`.

use std::path::Path;

use crate::Error;

/// Every embedded file, by `/`-separated path relative to `agents/`, such as
/// `standard/catalog.yaml`.
const FILES: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/embedded_agents.rs"));

/// The embedded file at `path` below `agents/`.
pub(crate) fn file(path: &str) -> Option<&'static [u8]> {
    FILES
        .iter()
        .find(|(embedded, _)| *embedded == path)
        .map(|(_, contents)| *contents)
}

/// Writes the embedded files below `folder`, a path relative to `agents/`, into `destination`,
/// keeping their places relative to `folder`.
pub(crate) fn extract(folder: &str, destination: &Path) -> Result<(), Error> {
    let prefix = format!("{folder}/");
    for (path, contents) in FILES {
        let Some(relative) = path.strip_prefix(&prefix) else {
            continue;
        };
        let path = destination.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents)?;
    }
    Ok(())
}

/// Copies the directory `source` into `destination`, as a checkout's copy of an embedded folder.
pub(crate) fn copy_directory(source: &Path, destination: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)
        .map_err(|error| Error::Invalid(format!("cannot read {}: {error}", source.display())))?
    {
        let entry = entry?;
        let path = entry.path();
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}
