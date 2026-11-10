//! Fingerprints of host directories, which tell whether a directory changed since its last copy.

use std::{
    io,
    path::{Path, PathBuf},
};

use ::sandbox::blocking::{self, Work};
use sha2::{Digest as _, Sha256};

use crate::Error;

/// What a directory held, judged by its entries' metadata rather than their contents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Fingerprint {
    /// Hash of the directory's own metadata and every entry's relative path, type, size,
    /// permissions, modification time and link target, and on Unix its inode and status change
    /// time. A program can set a file's
    /// modification time back after writing it, but not its status change time.
    pub(crate) digest: String,
    /// Regular files in the directory.
    pub(crate) files: u64,
    /// Bytes in those files.
    pub(crate) bytes: u64,
    /// The files in it that may hold real keys, as [`crate::exposed`] names them, which any agent
    /// that mounts the copy can read.
    pub(crate) exposed: Vec<PathBuf>,
}

/// Fingerprints `directory`. Reading only metadata keeps it quick, even for a large tree, and
/// finding the files that may hold real keys in the same walk keeps applying a volume to one walk.
pub(crate) async fn take(directory: PathBuf) -> Result<Fingerprint, Error> {
    blocking::run(Work::Tree, "fingerprinting the directory", move || {
        let mut fingerprint = Walk {
            hasher: Sha256::new(),
            files: 0,
            bytes: 0,
            exposed: Vec::new(),
        };
        // The directory's own metadata changes when an entry is added and removed again, which
        // its entries alone would not show.
        std::fs::metadata(&directory)
            .map(|metadata| fingerprint.metadata(&metadata))
            .and_then(|()| fingerprint.directory(&directory, Path::new("")))
            .map_err(|error| Error::Invalid(format!("cannot read {} to copy it: {error}", directory.display())))?;
        Ok(Fingerprint {
            digest: fingerprint
                .hasher
                .finalize()
                .iter()
                .fold(String::with_capacity(64), |mut output, byte| {
                    let _infallible = std::fmt::Write::write_fmt(&mut output, format_args!("{byte:02x}"));
                    output
                }),
            files: fingerprint.files,
            bytes: fingerprint.bytes,
            exposed: fingerprint
                .exposed
                .into_iter()
                .map(|relative| directory.join(relative))
                .collect(),
        })
    })
    .await?
}

struct Walk {
    hasher: Sha256,
    files: u64,
    bytes: u64,
    /// The files that may hold real keys, relative to the directory.
    exposed: Vec<PathBuf>,
}

impl Walk {
    fn directory(&mut self, directory: &Path, relative: &Path) -> io::Result<()> {
        let mut entries = std::fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            blocking::check().map_err(io::Error::other)?;
            let path = entry.path();
            let relative = relative.join(entry.file_name());
            let metadata = std::fs::symlink_metadata(&path)?;
            let kind = metadata.file_type();
            if !kind.is_dir() && crate::exposed::is_exposed_name(&entry.file_name()) {
                self.exposed.push(relative.clone());
            }
            let tag: &[u8] = if kind.is_dir() {
                b"d"
            } else if kind.is_file() {
                self.files += 1;
                self.bytes += metadata.len();
                b"f"
            } else if kind.is_symlink() {
                b"l"
            } else {
                b"o"
            };
            self.field(relative.as_os_str().as_encoded_bytes());
            self.field(tag);
            self.metadata(&metadata);
            if kind.is_symlink() {
                self.field(std::fs::read_link(&path)?.as_os_str().as_encoded_bytes());
            }
            if kind.is_dir() {
                self.directory(&path, &relative)?;
            }
        }
        Ok(())
    }

    /// Hashes an entry's size, permissions, modification time and identity.
    fn metadata(&mut self, metadata: &std::fs::Metadata) {
        self.field(&metadata.len().to_le_bytes());
        self.field(&permissions(metadata).to_le_bytes());
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .unwrap_or_default();
        self.field(&modified.as_nanos().to_le_bytes());
        self.field(&identity(metadata));
    }

    /// Hashes one field with its length, so neighbouring fields cannot run together.
    fn field(&mut self, bytes: &[u8]) {
        self.hasher.update((bytes.len() as u64).to_le_bytes());
        self.hasher.update(bytes);
    }
}

#[cfg(unix)]
fn permissions(metadata: &std::fs::Metadata) -> u32 {
    std::os::unix::fs::PermissionsExt::mode(&metadata.permissions())
}

#[cfg(not(unix))]
fn permissions(metadata: &std::fs::Metadata) -> u32 {
    u32::from(metadata.permissions().readonly())
}

/// The entry's inode and status change time, which change whenever it is written or replaced.
#[cfg(unix)]
fn identity(metadata: &std::fs::Metadata) -> [u8; 24] {
    use std::os::unix::fs::MetadataExt as _;
    let mut identity = [0; 24];
    identity[..8].copy_from_slice(&metadata.ino().to_le_bytes());
    identity[8..16].copy_from_slice(&metadata.ctime().to_le_bytes());
    identity[16..].copy_from_slice(&metadata.ctime_nsec().to_le_bytes());
    identity
}

#[cfg(not(unix))]
const fn identity(_metadata: &std::fs::Metadata) -> [u8; 0] {
    []
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_fingerprint_changes_with_the_tree_and_counts_its_files() {
        let directory = tempfile::tempdir().expect("directory");
        std::fs::create_dir(directory.path().join("app")).expect("app");
        std::fs::write(directory.path().join("app/one.txt"), b"one").expect("one");
        std::fs::write(directory.path().join("two.txt"), b"two!").expect("two");
        let first = take(directory.path().to_path_buf()).await.expect("fingerprint");
        assert_eq!((first.files, first.bytes), (2, 7));
        assert_eq!(take(directory.path().to_path_buf()).await.expect("again"), first);

        std::fs::write(directory.path().join("app/three.txt"), b"").expect("three");
        let added = take(directory.path().to_path_buf()).await.expect("added");
        assert_ne!(added.digest, first.digest);
        assert_eq!(added.files, 3);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_fingerprint_changes_when_a_file_is_added_and_removed_again() {
        let directory = tempfile::tempdir().expect("directory");
        std::fs::write(directory.path().join("app.txt"), b"app").expect("app");
        let first = take(directory.path().to_path_buf()).await.expect("fingerprint");
        // The status change time has a clock tick's resolution, which a quick change can fall within.
        std::thread::sleep(std::time::Duration::from_millis(20));

        let passing = directory.path().join("passing.txt");
        std::fs::write(&passing, b"briefly").expect("added");
        std::fs::remove_file(&passing).expect("removed");
        let after = take(directory.path().to_path_buf()).await.expect("after");
        assert_ne!(after.digest, first.digest);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_fingerprint_changes_when_a_file_is_rewritten_with_its_size_and_modification_time() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("value.txt");
        std::fs::write(&path, b"OLD VALUE\n").expect("value");
        let modified = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .expect("mtime");
        let first = take(directory.path().to_path_buf()).await.expect("fingerprint");
        // The status change time has a clock tick's resolution, which a quick rewrite can fall within.
        std::thread::sleep(std::time::Duration::from_millis(20));

        std::fs::write(&path, b"NEW VALUE\n").expect("rewritten value");
        std::fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_modified(modified))
            .expect("restored mtime");
        let rewritten = take(directory.path().to_path_buf()).await.expect("rewritten");
        assert_eq!(rewritten.bytes, first.bytes);
        assert_ne!(rewritten.digest, first.digest);
    }
}
