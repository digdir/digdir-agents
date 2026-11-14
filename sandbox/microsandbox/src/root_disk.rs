//! Host storage taken by a sandbox's root disk.
//!
//! Microsandbox keeps a sandbox's writable root filesystem as one sparse disk file in the
//! sandbox's runtime directory, so a single `stat` measures it, however many files the guest
//! holds and whether or not the sandbox runs.

use std::{io, path::Path};

use sandbox::{ByteQuantity, RootFilesystemUsage};

/// Measures the root disk at `path`, whose capacity is `recorded_capacity` until Microsandbox
/// creates it at the first start, when it takes no host storage yet.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the Backend measures with it in the next commit")
)]
pub(crate) async fn usage(path: &Path, recorded_capacity: ByteQuantity) -> io::Result<RootFilesystemUsage> {
    let _ = path;
    Ok(RootFilesystemUsage::new(0, recorded_capacity.bytes()))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::io::{Seek as _, SeekFrom, Write as _};

    use super::*;

    const MEBIBYTE: u64 = 1024 * 1024;

    /// A sparse disk reports the bytes written into it as allocated, and its length as capacity.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_sparse_disk_reports_its_allocated_bytes_and_length() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("rootfs.raw");
        let mut disk = std::fs::File::create(&path).expect("disk");
        disk.set_len(64 * MEBIBYTE).expect("sparse length");
        disk.seek(SeekFrom::Start(8 * MEBIBYTE)).expect("seek");
        disk.write_all(&vec![1; usize::try_from(MEBIBYTE).expect("size")])
            .expect("write");
        disk.sync_all().expect("sync");

        let usage = usage(&path, ByteQuantity::from_mebibytes(32).expect("recorded capacity"))
            .await
            .expect("usage");

        assert_eq!(usage.capacity_bytes(), 64 * MEBIBYTE);
        assert!(
            (MEBIBYTE..8 * MEBIBYTE).contains(&usage.allocated_bytes()),
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
