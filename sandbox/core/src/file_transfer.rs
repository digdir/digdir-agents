//! Streaming transfer of regular files to and from a running sandbox.

use std::{path::Path, pin::Pin};

use tokio::{
    fs::File,
    io::{AsyncRead, AsyncWriteExt as _},
};

use crate::{Error, SandboxHandle, SandboxPath};

/// An owned, non-`Send` asynchronous byte reader.
pub type ByteReader = Pin<Box<dyn AsyncRead>>;

/// Streams one host file into a running sandbox.
///
/// # Errors
///
/// Returns an error when the host file cannot be opened or the Backend cannot
/// complete the transfer.
pub async fn copy_file_to_sandbox(
    sandbox: &SandboxHandle,
    host_path: &Path,
    sandbox_path: &SandboxPath,
) -> Result<(), Error> {
    let source = File::open(host_path).await.map_err(|source| Error::Io {
        operation: "open host file for sandbox transfer",
        source,
    })?;

    sandbox.write_file(sandbox_path, Box::pin(source)).await
}

/// Streams one file from a running sandbox into a host file.
///
/// # Errors
///
/// Returns an error when the Backend cannot read the sandbox file or the host
/// file cannot be written.
pub async fn copy_file_from_sandbox(
    sandbox: &SandboxHandle,
    sandbox_path: &SandboxPath,
    host_path: &Path,
) -> Result<(), Error> {
    let mut source = sandbox.read_file(sandbox_path).await?;
    let mut destination = File::create(host_path).await.map_err(|source| Error::Io {
        operation: "create host file for sandbox transfer",
        source,
    })?;

    tokio::io::copy(&mut source, &mut destination)
        .await
        .map_err(|source| Error::Io {
            operation: "copy sandbox file to host",
            source,
        })?;
    destination.flush().await.map_err(|source| Error::Io {
        operation: "flush host file copied from sandbox",
        source,
    })
}
