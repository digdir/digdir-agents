//! Reading an Agent's instructions, skills and home from the host.

use std::path::Path;

use crate::{Error, control_plane, harness};

/// Concatenates the instruction files in manifest order, each terminated by a newline and
/// separated by a blank line, so independent documents read as sections of one file.
pub(super) async fn read_instructions(record: &control_plane::AgentRecord) -> Result<Option<Vec<u8>>, Error> {
    let mut combined = Vec::new();
    for (index, spec) in record.agent.spec.instructions.iter().enumerate() {
        let source = crate::manifest::resolve_host_path(
            &record.source_directory,
            &spec.source,
            &format!("spec.instructions[{index}].source"),
        )?;
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
            .ok_or_else(|| Error::Invalid(format!("{field} must end in the skill's directory name")))?
            .to_owned();
        let source = resolve_source(&record.source_directory, &spec.source, &field)?;
        let skill = tokio::task::spawn_blocking(move || harness::Skill::read(name, &source, &field))
            .await
            .map_err(|error| Error::Daemon(format!("Agent skill scan task failed: {error}")))??;
        skills.push(skill);
    }
    Ok(skills)
}

pub(super) fn resolve_source(
    manifest_directory: &Path,
    source: &Path,
    field: &str,
) -> Result<std::path::PathBuf, Error> {
    let source = std::fs::canonicalize(crate::manifest::resolve_host_path(manifest_directory, source, field)?)?;
    if !source.is_dir() {
        return Err(Error::Invalid(format!("{field} must identify a directory")));
    }
    Ok(source)
}
