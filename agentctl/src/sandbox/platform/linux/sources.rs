//! Reading an Agent's instructions, skills and home from the host.

use std::path::Path;

use ignore::WalkBuilder;

use crate::{Error, control_plane, harness};

/// Concatenates the instruction files in manifest order, each terminated by a newline and
/// separated by a blank line, so independent documents read as sections of one file.
pub(super) async fn read_instructions(record: &control_plane::AgentRecord) -> Result<Option<Vec<u8>>, Error> {
    let mut combined = Vec::new();
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
        let contents = tokio::fs::read(source).await?;
        if !combined.is_empty() {
            combined.push(b'\n');
        }
        combined.extend_from_slice(contents.strip_suffix(b"\n").unwrap_or(&contents));
        combined.push(b'\n');
    }
    Ok((!combined.is_empty()).then_some(combined))
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
