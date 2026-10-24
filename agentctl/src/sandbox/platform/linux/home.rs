//! Carrying `spec.home` into the Sandbox.

use std::{io::Cursor, path::Path};

use ::sandbox::{SandboxHandle, SandboxPath};

use crate::Error;

use super::{
    HOME, run_checked,
    sources::{resolve_source, walk_source},
};

const HOME_ARCHIVE: &str = "/tmp/agent-home.tar";

pub(super) async fn archive_home(
    manifest_directory: std::path::PathBuf,
    source: std::path::PathBuf,
) -> Result<Vec<u8>, Error> {
    let source = resolve_source(&manifest_directory, &source, "spec.home.source")?;
    archive_directory(source, "spec.home.source".to_owned()).await
}

/// Archives a resolved host directory on a blocking thread; `field` names the manifest field in errors.
async fn archive_directory(source: std::path::PathBuf, field: String) -> Result<Vec<u8>, Error> {
    let task = format!("Agent {field} scan task failed");
    tokio::task::spawn_blocking(move || archive_directory_blocking(&source, &field))
        .await
        .map_err(|error| Error::Daemon(format!("{task}: {error}")))?
}

fn archive_directory_blocking(source: &Path, field: &str) -> Result<Vec<u8>, Error> {
    let mut archive = tar::Builder::new(Vec::new());
    walk_source(source, field, |relative, path, is_dir| {
        if is_dir {
            archive.append_dir(relative, path)?;
        } else {
            archive.append_path_with_name(path, relative)?;
        }
        Ok(())
    })?;
    archive.into_inner().map_err(Error::from)
}

pub(super) async fn sync_home(sandbox: &SandboxHandle, archive: Vec<u8>) -> Result<(), Error> {
    sandbox
        .write_file(&SandboxPath::new(HOME_ARCHIVE), Box::pin(Cursor::new(archive)))
        .await?;
    run_checked(sandbox, "/usr/bin/tar", ["-xf", HOME_ARCHIVE, "-C", HOME]).await
}
