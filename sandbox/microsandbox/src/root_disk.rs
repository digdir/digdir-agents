//! Host storage taken by a sandbox's root disk.
//!
//! Microsandbox keeps a sandbox's writable root filesystem as one sparse disk file in the
//! sandbox's runtime directory, so measuring that one file measures it, however many files the guest
//! holds and whether or not the sandbox runs.

use std::{
    io,
    path::{Path, PathBuf},
};

use sandbox::{ByteQuantity, RootFilesystemMode, RootFilesystemUsage};

/// The disk holding a whole direct root filesystem, cloned from the Image's flat root.
const DIRECT_DISK: &str = "rootfs.raw";
/// The disk holding a layered root filesystem's writable upper layer.
const LAYERED_DISK: &str = "upper.ext4";

/// The root disk Microsandbox keeps in a sandbox's runtime directory for a root filesystem of
/// `mode`. Microsandbox names them without exporting the names.
pub(crate) fn path(runtime_directory: &Path, mode: RootFilesystemMode) -> Option<PathBuf> {
    match mode {
        RootFilesystemMode::Direct => Some(runtime_directory.join(DIRECT_DISK)),
        RootFilesystemMode::Layered => Some(runtime_directory.join(LAYERED_DISK)),
        _ => None,
    }
}

/// Measures the root disk at `path`, whose capacity is `recorded_capacity` until Microsandbox
/// creates it at the first start, when it takes no host storage yet.
pub(crate) async fn usage(path: &Path, recorded_capacity: ByteQuantity) -> io::Result<RootFilesystemUsage> {
    let path = path.to_owned();
    common::blocking::run("measuring a root disk", move || {
        let capacity = match std::fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(RootFilesystemUsage::new(0, recorded_capacity.bytes()));
            }
            Err(error) => return Err(error),
        };
        // Measured as Microsandbox measures its own disks, so that both count the same bytes on
        // every host. For a sparse file this leaves out the holes.
        let allocated = microsandbox_utils::extent::allocated_file_bytes(&path)?;
        Ok(RootFilesystemUsage::new(allocated, capacity))
    })
    .await
    .map_err(io::Error::other)?
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    /// A sparse disk reports the bytes written into it as allocated, and its length as capacity.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_sparse_disk_reports_its_allocated_bytes_and_length() {
        use std::io::Write as _;

        const MEBIBYTE: u64 = 1024 * 1024;

        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("rootfs.raw");
        let mut disk = std::fs::File::create(&path).expect("disk");
        disk.set_len(64 * MEBIBYTE).expect("sparse length");
        // Written at the start, since APFS also allocates a hole before the data.
        disk.write_all(&vec![1; usize::try_from(MEBIBYTE).expect("size")])
            .expect("write");
        disk.sync_all().expect("sync");

        let usage = usage(&path, ByteQuantity::from_mebibytes(32).expect("recorded capacity"))
            .await
            .expect("usage");

        assert_eq!(usage.capacity_bytes(), 64 * MEBIBYTE);
        assert!(
            (MEBIBYTE..32 * MEBIBYTE).contains(&usage.allocated_bytes()),
            "allocated {} bytes",
            usage.allocated_bytes()
        );
    }

    /// Before the first start there is no disk, which takes nothing and will have the recorded capacity.
    #[tokio::test]
    async fn a_disk_not_created_yet_takes_nothing() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let capacity = ByteQuantity::from_mebibytes(32).expect("recorded capacity");

        let usage = usage(&directory.path().join("upper.ext4"), capacity)
            .await
            .expect("usage");

        assert_eq!(usage, RootFilesystemUsage::new(0, capacity.bytes()));
    }
}
