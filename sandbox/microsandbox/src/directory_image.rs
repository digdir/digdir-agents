//! Read-only copies of host directories, which a sandbox mounts as EROFS disk images.
//!
//! A bind mount asks the host for every lookup, open and read, which a guest walking or
//! searching a large tree repeats on every pass. An image is a file the guest reads as a
//! block device, so it caches the whole tree and stops asking the host.

use std::{
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use microsandbox_image::{
    erofs,
    tree::{DataSpool, DirectoryNode, FileTree, InodeMetadata, RegularFileId, RegularFileNode, SymlinkNode, TreeNode},
};
use sandbox::{Error, ProgressUnit, SandboxProgress, volume::Owner};
use tokio::sync::mpsc;

use crate::error;

/// Size of the reads that copy file contents into the spool.
const COPY_BUFFER: usize = 1 << 20;
/// Shortest interval between reports of the bytes copied, so a tree of small files does not
/// report once per file.
const REPORT_INTERVAL: Duration = Duration::from_millis(100);

/// How far a build has come, sent from the thread that builds the image.
enum Stage {
    /// The tree was read, and its files hold this many bytes.
    Scanned(u64),
    /// This many bytes of the files are copied.
    Copied(u64),
    /// Every file is copied, and the image is being written.
    Writing,
}

/// Builds an image of `source` at `output`, replacing any image there, and reports copying the
/// files, measured in bytes, and writing the image as steps of the caller's phase.
///
/// Symbolic links are copied as links, not followed, so the copy holds nothing from outside
/// `source`. Other special files are left out. Every entry is owned by `owner`, as the guest's
/// users are not the host's, and readable by everyone.
pub(crate) async fn build(
    source: PathBuf,
    owner: Owner,
    output: PathBuf,
    progress: &SandboxProgress,
) -> Result<(), Error> {
    let (stages, mut received) = mpsc::unbounded_channel();
    let build = tokio::task::spawn_blocking(move || build_blocking(&source, owner, &output, &stages));
    let mut copying = None;
    let mut writing = None;
    // The channel closes when the build ends. A step still open then ends as failed with it.
    while let Some(stage) = received.recv().await {
        match stage {
            Stage::Scanned(total) => {
                let step = progress
                    .start_measured_step("Read files", ProgressUnit::Bytes, Some(total))
                    .await;
                copying = Some((step, total));
            }
            Stage::Copied(bytes) => {
                if let Some((step, total)) = &copying {
                    step.report(bytes.min(*total), Some(*total)).await;
                }
            }
            Stage::Writing => {
                if let Some((step, _)) = copying.take() {
                    step.complete().await;
                }
                writing = Some(progress.start_step("Write image").await);
            }
        }
    }
    build.await.map_err(error::backend)??;
    if let Some(step) = writing {
        step.complete().await;
    }
    Ok(())
}

fn build_blocking(
    source: &Path,
    owner: Owner,
    output: &Path,
    stages: &mpsc::UnboundedSender<Stage>,
) -> Result<(), Error> {
    let partial = output.with_extension("partial");
    let spool_path = output.with_extension("spool");
    let result = (|| {
        let mut tree = FileTree::new();
        tree.root.metadata = metadata(
            &std::fs::metadata(source).map_err(|error| read_error(source, &error))?,
            owner,
            true,
        );
        // The tree is read before any file, so the bytes to copy are known when copying starts.
        let mut files = Vec::new();
        add_directory(&mut tree, &mut files, source, owner, &[])?;
        let _ = stages.send(Stage::Scanned(files.iter().map(|file| file.metadata.len()).sum()));
        let mut spool = DataSpool::new(&spool_path).map_err(|source| error::io("create image spool", source))?;
        let mut copied = Copied {
            stages,
            bytes: 0,
            reported: Instant::now(),
        };
        for file in files {
            let node = TreeNode::RegularFile(RegularFileNode {
                id: RegularFileId::new(),
                metadata: metadata(&file.metadata, owner, false),
                xattrs: Vec::new(),
                data: copy_contents(&mut spool, &file.host_path, &mut copied)?,
                nlink: 1,
            });
            tree.insert(&file.entry_path, node)
                .map_err(|error| Error::Backend(format!("cannot copy {}: {error}", file.host_path.display())))?;
        }
        let _ = stages.send(Stage::Copied(copied.bytes));
        let _ = stages.send(Stage::Writing);
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

/// A regular file found in the tree, whose contents are copied once the whole tree is read.
struct File {
    entry_path: Vec<u8>,
    host_path: PathBuf,
    metadata: std::fs::Metadata,
}

/// The bytes copied so far, and when they were last reported.
struct Copied<'a> {
    stages: &'a mpsc::UnboundedSender<Stage>,
    bytes: u64,
    reported: Instant,
}

impl Copied<'_> {
    fn add(&mut self, bytes: usize) {
        self.bytes += bytes as u64;
        if self.reported.elapsed() >= REPORT_INTERVAL {
            self.reported = Instant::now();
            let _ = self.stages.send(Stage::Copied(self.bytes));
        }
    }
}

/// Adds the directories and symbolic links below `directory` to the tree, and collects its
/// regular files in `files`.
fn add_directory(
    tree: &mut FileTree,
    files: &mut Vec<File>,
    directory: &Path,
    owner: Owner,
    path: &[u8],
) -> Result<(), Error> {
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
            TreeNode::Directory(DirectoryNode::new(metadata(&host_metadata, owner, true)))
        } else if file_type.is_file() {
            files.push(File {
                entry_path,
                host_path,
                metadata: host_metadata,
            });
            continue;
        } else if file_type.is_symlink() {
            let target = std::fs::read_link(&host_path).map_err(|error| read_error(&host_path, &error))?;
            TreeNode::Symlink(SymlinkNode {
                metadata: InodeMetadata {
                    mode: 0o777,
                    ..metadata(&host_metadata, owner, false)
                },
                target: name_bytes(target.as_os_str(), &host_path)?,
            })
        } else {
            continue;
        };
        tree.insert(&entry_path, node)
            .map_err(|error| Error::Backend(format!("cannot copy {}: {error}", host_path.display())))?;
        if file_type.is_dir() {
            add_directory(tree, files, &host_path, owner, &entry_path)?;
        }
    }
    Ok(())
}

fn copy_contents(
    spool: &mut DataSpool,
    path: &Path,
    copied: &mut Copied<'_>,
) -> Result<microsandbox_image::tree::FileData, Error> {
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
        copied.add(read);
    }
    Ok(spool.data_ref(offset, spool.current_offset() - offset))
}

/// The guest-visible metadata of a copied entry: owned by `owner`, with the host's permission
/// bits, readable by everyone, and searchable by everyone for directories.
fn metadata(host: &std::fs::Metadata, owner: Owner, directory: bool) -> InodeMetadata {
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
        uid: owner.uid,
        gid: owner.gid,
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
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let recorded = events.clone();
        let reporter = sandbox::ProgressReporter::from_callback(move |event| recorded.borrow_mut().push(event));
        let phase = reporter.start_phase(sandbox::Phase::new("copy", "Copy")).await;

        build(source, Owner::default(), output.clone(), &reporter.steps())
            .await
            .unwrap();

        phase.complete().await;
        let mut progress = sandbox::progress::Progress::new();
        for event in events.borrow().iter() {
            progress.apply(event);
        }
        let steps: Vec<_> = progress.finished()[0]
            .steps
            .iter()
            .map(|step| {
                (
                    step.name.as_str(),
                    step.measurement
                        .map(|measurement| (measurement.completed, measurement.total)),
                )
            })
            .collect();
        let bytes = 3 + COPY_BUFFER as u64 * 2 + 5;
        assert_eq!(
            steps,
            [("Read files", Some((bytes, Some(bytes)))), ("Write image", None)]
        );

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
