//! Reading directories on the host that the platform copies elsewhere, such as an Agent's home and skills.

use std::path::Path;

use ignore::WalkBuilder;

use crate::Error;

/// Visits every entry below `source` with its relative path, rejecting symbolic links.
pub(crate) fn walk_source(
    source: &Path,
    field: &str,
    mut visit: impl FnMut(&Path, &Path, bool) -> Result<(), Error>,
) -> Result<(), Error> {
    for result in WalkBuilder::new(source)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_exclude(false)
        .parents(false)
        .follow_links(false)
        .build()
    {
        let entry = result.map_err(|error| Error::Invalid(format!("cannot traverse {field}: {error}")))?;
        let relative = entry
            .path()
            .strip_prefix(source)
            .map_err(|_| Error::Invalid(format!("{field} traversal escaped its root")))?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        // Only directories and regular files are carried into the Sandbox. A symbolic link would
        // point at host content outside the source, and reading a FIFO or device would block setup.
        let kind = entry
            .file_type()
            .ok_or_else(|| Error::Invalid(format!("{field} entry {} has no file type", relative.display())))?;
        if kind.is_symlink() {
            return Err(Error::Invalid(format!(
                "{field} contains unsupported symbolic link {}",
                relative.display()
            )));
        }
        if !kind.is_dir() && !kind.is_file() {
            return Err(Error::Invalid(format!(
                "{field} contains unsupported non-regular file {}",
                relative.display()
            )));
        }
        visit(relative, entry.path(), kind.is_dir())?;
    }
    Ok(())
}
