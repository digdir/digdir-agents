//! The agent builder, the agent `agentctl new` runs to make a new agent.
//!
//! It asks what the agent is for and on which repository, writes its definition and tries it. It
//! is not one of the standard agents, which are kinds of work to start from, but the way to make an
//! agent of your own.
//!
//! Its definition is `agents/agent-builder/`, on a published image like a standard agent's, and the
//! build embeds it as it does the catalog. `AGENTCTL_BUILDER_DIR` points at a checkout's
//! `agents/agent-builder` instead, to try a change without a rebuild.

use std::path::{Path, PathBuf};

use crate::{Error, embedded, manifest::MANIFEST_FILE};

/// The variable that replaces the embedded agent builder with a directory.
pub const BUILDER_DIRECTORY_VARIABLE: &str = "AGENTCTL_BUILDER_DIR";

/// Its folder below `agents/`, and the folder it is written to.
const FOLDER: &str = "agent-builder";

/// Writes the agent builder's definition into `destination` and returns the path of its manifest
/// there.
///
/// # Errors
///
/// Returns an error when the definition has no manifest or a file cannot be written.
pub fn extract(destination: &Path) -> Result<PathBuf, Error> {
    let folder = destination.join(FOLDER);
    match std::env::var_os(BUILDER_DIRECTORY_VARIABLE).filter(|value| !value.is_empty()) {
        Some(directory) => embedded::copy_directory(Path::new(&directory), &folder)?,
        None => embedded::extract(FOLDER, &folder)?,
    }
    let manifest = folder.join(MANIFEST_FILE);
    if !manifest.is_file() {
        return Err(Error::Invalid(format!("the agent builder has no {MANIFEST_FILE}")));
    }
    Ok(manifest)
}
