//! Reading an Agent's instructions, skills and home from the host.

use std::path::Path;

use ignore::WalkBuilder;

use crate::{Error, control_plane, harness};

/// The platform's instructions in `about`, then the host-wide instructions in `host` when that
/// file exists, then the agent's own in manifest order: each terminated by a newline and separated
/// by a blank line, so independent documents read as sections of one file.
pub(super) async fn read_instructions(
    record: &control_plane::AgentRecord,
    about: &str,
    host: Option<&Path>,
) -> Result<Vec<u8>, Error> {
    let mut combined = Vec::new();
    append_instructions(&mut combined, about.as_bytes());
    if let Some(contents) = read_host_instructions(host).await? {
        append_instructions(&mut combined, &contents);
    }
    for (index, spec) in record.agent.spec.instructions.iter().enumerate() {
        let source = if spec.source.is_absolute() {
            spec.source.clone()
        } else {
            record.source_directory.join(&spec.source)
        };
        let metadata = tokio::fs::metadata(&source).await?;
        if !metadata.is_file() {
            return Err(Error::Invalid(format!(
                "spec.instructions[{index}].source must identify a file"
            )));
        }
        append_instructions(&mut combined, &tokio::fs::read(source).await?);
    }
    Ok(combined)
}

/// The host-wide instructions file's contents; a missing file, or a directory or anything else
/// that is not a file at that path, adds nothing. Setup fails naming the file it cannot read.
async fn read_host_instructions(host: Option<&Path>) -> Result<Option<Vec<u8>>, Error> {
    let Some(host) = host else {
        return Ok(None);
    };
    let unreadable = |error: std::io::Error| {
        Error::SandboxSetup(format!(
            "cannot read host-wide instructions {}: {error}",
            host.display()
        ))
    };
    match tokio::fs::metadata(host).await {
        Ok(metadata) if metadata.is_file() => tokio::fs::read(host).await.map(Some).map_err(unreadable),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(unreadable(error)),
    }
}

/// Appends one instructions document, separated from what came before by a blank line; an empty
/// one adds nothing.
fn append_instructions(combined: &mut Vec<u8>, contents: &[u8]) {
    let contents = contents.strip_suffix(b"\n").unwrap_or(contents);
    if contents.trim_ascii().is_empty() {
        return;
    }
    if !combined.is_empty() {
        combined.push(b'\n');
    }
    combined.extend_from_slice(contents);
    combined.push(b'\n');
}

pub(super) async fn read_skills(record: &control_plane::AgentRecord) -> Result<Vec<harness::Skill>, Error> {
    let mut skills = Vec::with_capacity(record.agent.spec.skills.len());
    for (index, spec) in record.agent.spec.skills.iter().enumerate() {
        let field = format!("spec.skills[{index}].source");
        let name = spec
            .name()
            .ok_or_else(|| Error::Invalid(format!("{field} must end in the skill's directory name")))?;
        let source = resolve_source(&record.source_directory, &spec.source, &field)?;
        if !source.join("SKILL.md").is_file() {
            return Err(Error::Invalid(format!("{field} must contain SKILL.md")));
        }
        let files = tokio::task::spawn_blocking(move || read_skill_files(&source, &field))
            .await
            .map_err(|error| Error::Daemon(format!("Agent skill scan task failed: {error}")))??;
        skills.push(harness::Skill {
            name: name.to_owned(),
            files,
        });
    }
    Ok(skills)
}

fn read_skill_files(source: &Path, field: &str) -> Result<Vec<harness::SkillFile>, Error> {
    let mut files = Vec::new();
    walk_source(source, field, |relative, path, is_dir| {
        if is_dir {
            return Ok(());
        }
        let relative_path = relative
            .components()
            .map(|component| component.as_os_str().to_str())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| Error::Invalid(format!("{field} contains a non-UTF-8 file name")))?
            .join("/");
        files.push(harness::SkillFile {
            relative_path,
            contents: std::fs::read(path)?,
        });
        Ok(())
    })?;
    Ok(files)
}

/// Visits every entry below `source` with its relative path, rejecting symbolic links.
pub(super) fn walk_source(
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

pub(super) fn resolve_source(
    manifest_directory: &Path,
    source: &Path,
    field: &str,
) -> Result<std::path::PathBuf, Error> {
    let source = if source.is_absolute() {
        source.to_path_buf()
    } else {
        manifest_directory.join(source)
    };
    let source = std::fs::canonicalize(source)?;
    if !source.is_dir() {
        return Err(Error::Invalid(format!("{field} must identify a directory")));
    }
    Ok(source)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn host_instructions_that_are_missing_or_not_a_file_add_nothing() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let host = directory.path().join("instructions.md");
        assert!(read_host_instructions(None).await.expect("no file").is_none());
        assert!(read_host_instructions(Some(&host)).await.expect("missing").is_none());
        std::fs::create_dir(&host).expect("directory at the path");
        assert!(read_host_instructions(Some(&host)).await.expect("directory").is_none());
        std::fs::remove_dir(&host).expect("remove directory");
        std::fs::write(&host, "Host rules\n").expect("host instructions");
        assert_eq!(
            read_host_instructions(Some(&host)).await.expect("file"),
            Some(b"Host rules\n".to_vec())
        );
    }

    /// A path through a file fails to read for any user, root included, unlike a permission.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn host_instructions_that_cannot_be_read_fail_naming_the_file() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        std::fs::write(directory.path().join("home"), "").expect("a file where a directory belongs");
        let host = directory.path().join("home").join("instructions.md");
        let error = read_host_instructions(Some(&host)).await.expect_err("unreadable");
        let text = error.to_string();
        assert!(
            text.starts_with(&format!(
                "Agent Sandbox setup failed: cannot read host-wide instructions {}: ",
                host.display()
            )),
            "{text}"
        );
    }
}
