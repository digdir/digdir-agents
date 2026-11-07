//! Read-only copies of host directories, which a sandbox mounts as EROFS disk images.
//!
//! A bind mount asks the host for every lookup, open and read, which a guest walking or
//! searching a large tree repeats on every pass. An image is a file the guest reads as a
//! block device, so it caches the whole tree and stops asking the host.

use std::{
    io::Read,
    path::{Path, PathBuf},
};

use microsandbox_image::{
    erofs,
    tree::{DataSpool, DirectoryNode, FileTree, InodeMetadata, RegularFileId, RegularFileNode, SymlinkNode, TreeNode},
};
use sandbox::Error;

use crate::error;

/// Size of the reads that copy file contents into the spool.
const COPY_BUFFER: usize = 1 << 20;

/// Builds an image of `source` at `output`, replacing any image there.
///
/// Symbolic links are copied as links, not followed, so the copy holds nothing from outside
/// `source`. Other special files are left out. Every entry is owned by root and readable by
/// everyone, because the guest's users are not the host's and the copy cannot be changed.
pub(crate) async fn build(source: PathBuf, output: PathBuf) -> Result<(), Error> {
    tokio::task::spawn_blocking(move || build_blocking(&source, &output))
        .await
        .map_err(error::backend)?
}

fn build_blocking(source: &Path, output: &Path) -> Result<(), Error> {
    let partial = output.with_extension("partial");
    let spool_path = output.with_extension("spool");
    let result = (|| {
        let mut spool = DataSpool::new(&spool_path).map_err(|source| error::io("create image spool", source))?;
        let mut tree = FileTree::new();
        tree.root.metadata = metadata(
            &std::fs::metadata(source).map_err(|error| read_error(source, &error))?,
            true,
        );
        add_directory(&mut tree, &mut spool, source, &[])?;
        erofs::write_erofs(&tree, &partial)
            .map_err(|error| Error::Backend(format!("cannot write the image of {}: {error}", source.display())))?;
        std::fs::rename(&partial, output).map_err(|source| error::io("store directory image", source))
    })();
    // The spool holds a second copy of the contents, which the image no longer needs.
    let _ = std::fs::remove_file(&spool_path);
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

fn add_directory(tree: &mut FileTree, spool: &mut DataSpool, directory: &Path, path: &[u8]) -> Result<(), Error> {
    let mut entries = std::fs::read_dir(directory)
        .map_err(|error| read_error(directory, &error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| read_error(directory, &error))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let host_path = entry.path();
        let mut entry_path = path.to_vec();
        if !entry_path.is_empty() {
            entry_path.push(b'/');
        }
        entry_path.extend_from_slice(&name_bytes(&entry.file_name(), &host_path)?);
        let host_metadata = std::fs::symlink_metadata(&host_path).map_err(|error| read_error(&host_path, &error))?;
        let file_type = host_metadata.file_type();
        let node = if file_type.is_dir() {
            TreeNode::Directory(DirectoryNode::new(metadata(&host_metadata, true)))
        } else if file_type.is_file() {
            TreeNode::RegularFile(RegularFileNode {
                id: RegularFileId::new(),
                metadata: metadata(&host_metadata, false),
                xattrs: Vec::new(),
                data: copy_contents(spool, &host_path)?,
                nlink: 1,
            })
        } else if file_type.is_symlink() {
            let target = std::fs::read_link(&host_path).map_err(|error| read_error(&host_path, &error))?;
            TreeNode::Symlink(SymlinkNode {
                metadata: InodeMetadata {
                    mode: 0o777,
                    ..metadata(&host_metadata, false)
                },
                target: name_bytes(target.as_os_str(), &host_path)?,
            })
        } else {
            continue;
        };
        tree.insert(&entry_path, node)
            .map_err(|error| Error::Backend(format!("cannot copy {}: {error}", host_path.display())))?;
        if file_type.is_dir() {
            add_directory(tree, spool, &host_path, &entry_path)?;
        }
    }
    Ok(())
}

fn copy_contents(spool: &mut DataSpool, path: &Path) -> Result<microsandbox_image::tree::FileData, Error> {
    let mut file = std::fs::File::open(path).map_err(|error| read_error(path, &error))?;
    let offset = spool.current_offset();
    let mut buffer = vec![0; COPY_BUFFER];
    loop {
        let read = file.read(&mut buffer).map_err(|error| read_error(path, &error))?;
        if read == 0 {
            break;
        }
        spool
            .write_chunk(&buffer[..read])
            .map_err(|source| error::io("write image spool", source))?;
    }
    Ok(spool.data_ref(offset, spool.current_offset() - offset))
}

/// The guest-visible metadata of a copied entry: the host's permission bits, readable by
/// everyone, and searchable by everyone for directories.
fn metadata(host: &std::fs::Metadata, directory: bool) -> InodeMetadata {
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        // Set-ID bits would grant nothing the guest should act on.
        u16::try_from(host.permissions().mode() & 0o1777).unwrap_or(0o644)
    };
    #[cfg(not(unix))]
    let mode = if directory { 0o755 } else { 0o644 };
    let readable = if directory { 0o555 } else { 0o444 };
    let mtime = host
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .unwrap_or_default();
    InodeMetadata {
        uid: 0,
        gid: 0,
        mode: mode | readable,
        mtime: mtime.as_secs(),
        mtime_nsec: mtime.subsec_nanos(),
    }
}

/// The bytes the guest sees for a host name. Elsewhere than on Unix, a name must be Unicode.
#[cfg_attr(
    unix,
    allow(
        clippy::unnecessary_wraps,
        reason = "only other systems can have names the guest cannot take"
    )
)]
fn name_bytes(name: &std::ffi::OsStr, path: &Path) -> Result<Vec<u8>, Error> {
    #[cfg(unix)]
    {
        let _ = path;
        Ok(std::os::unix::ffi::OsStrExt::as_bytes(name).to_vec())
    }
    #[cfg(not(unix))]
    {
        name.to_str()
            .map(|name| name.replace('\\', "/").into_bytes())
            .ok_or_else(|| Error::Backend(format!("cannot copy {}: its name is not Unicode", path.display())))
    }
}

fn read_error(path: &Path, error: &std::io::Error) -> Error {
    Error::Backend(format!(
        "cannot read {} to copy it into the sandbox: {error}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_image_holds_the_directory_tree() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        std::fs::create_dir_all(source.join("nested/deeper")).unwrap();
        std::fs::write(source.join("top.txt"), b"top").unwrap();
        std::fs::write(source.join("nested/deeper/large.bin"), vec![7_u8; COPY_BUFFER * 2 + 5]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("../top.txt", source.join("nested/link")).unwrap();
        let output = directory.path().join("image.erofs");

        build(source, output.clone()).await.unwrap();

        let mut reader = erofs::ErofsReader::new(std::fs::File::open(&output).unwrap()).unwrap();
        assert_eq!(reader.read_file("/top.txt").unwrap(), b"top");
        assert_eq!(
            reader.read_file("/nested/deeper/large.bin").unwrap().len(),
            COPY_BUFFER * 2 + 5
        );
        #[cfg(unix)]
        assert_eq!(reader.read_link("/nested/link").unwrap(), b"../top.txt");
        assert!(!output.with_extension("spool").exists());
    }
}
