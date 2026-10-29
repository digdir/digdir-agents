//! Skill installation shared by the Linux harness bootstraps.
//!
//! A skill is a directory holding `SKILL.md`, read on the host from `spec.skills`. Each harness
//! discovers skills in its own user-level directory, so the adapter chooses the root and this
//! module does the placement.

use std::collections::BTreeSet;

use sandbox::{SandboxHandle, SandboxPath, execution::ExecutionSpec};

use crate::{
    Error,
    sandbox::platform::files::{read_existing, write_if_changed},
};

/// Names the skills setup placed below a root, one per line. The image and the agent may put skills
/// of their own there too, so this record is how a later pass tells the ones it placed, and may
/// remove once they are no longer declared, from those it must leave alone.
const PLACED: &str = ".agentd-skills";
/// The most of the record setup reads back. A longer one, which only the sandbox user can have
/// written, is read as empty.
const MAX_PLACED: usize = 64 * 1024;

/// One skill directory read on the host, keyed by its directory name.
pub(crate) struct Skill {
    pub(crate) name: String,
    /// Regular files below the skill directory, as `/`-separated relative paths.
    pub(crate) files: Vec<SkillFile>,
}

/// One regular file of a skill.
pub(crate) struct SkillFile {
    pub(crate) relative_path: String,
    pub(crate) contents: Vec<u8>,
}

/// The skill every agent gets for driving agents with `agentctl`, nested ones included, which the
/// agent sandbox section of its instructions points to.
pub(crate) fn platform_skill() -> Skill {
    Skill {
        name: "agentctl".into(),
        files: vec![SkillFile {
            relative_path: "SKILL.md".into(),
            // CI skips Markdown changes, except under agentctl/skills/. Bundling Markdown from
            // elsewhere needs it added to `bundled` in ci.yml and the paths in agentctl-image.yml.
            contents: include_bytes!("../../skills/agentctl/SKILL.md").to_vec(),
        }],
    }
}

/// Places every skill below `root` as `root/<name>/<relative path>`, owned by the image user, and
/// removes the skill directories an earlier pass placed that `skills` no longer holds.
///
/// Harnesses watch their skill directories live, so a file whose contents are already in place
/// is left alone rather than rewritten. Directories are created as the image user. Runtime file
/// transfer writes as the sandbox supervisor, so exactly the managed files are chowned
/// afterwards: no recursive ownership walk.
pub(super) async fn install_linux(sandbox: &SandboxHandle, root: &str, skills: &[Skill]) -> Result<(), Error> {
    run_checked(sandbox, "/usr/bin/mkdir", ["-p".to_owned(), root.to_owned()]).await?;
    let record = format!("{root}/{PLACED}");
    let placed = read_existing(sandbox, &record, MAX_PLACED)
        .await
        .map(|contents| placed_names(&contents))
        .unwrap_or_default();
    let declared: BTreeSet<String> = skills.iter().map(|skill| skill.name.clone()).collect();
    // Recorded before anything is placed, so a pass that fails part way still finds what it placed.
    write_placed(sandbox, &record, &placed.union(&declared).cloned().collect()).await?;
    let stale: Vec<String> = placed
        .difference(&declared)
        .map(|name| format!("{root}/{name}"))
        .collect();
    if !stale.is_empty() {
        let mut remove = vec!["/usr/bin/rm".to_owned(), "-rf".to_owned(), "--".to_owned()];
        remove.extend(stale);
        run_checked(sandbox, "/usr/bin/sudo", remove).await?;
    }
    for skill in skills {
        let target = format!("{root}/{}", skill.name);
        run_checked(sandbox, "/usr/bin/mkdir", ["-p".to_owned(), target.clone()]).await?;
        let mut managed = Vec::with_capacity(skill.files.len());
        for file in &skill.files {
            let path = format!("{target}/{}", file.relative_path);
            if let Some((parent, _)) = file.relative_path.rsplit_once('/') {
                run_checked(
                    sandbox,
                    "/usr/bin/mkdir",
                    ["-p".to_owned(), format!("{target}/{parent}")],
                )
                .await?;
            }
            write_if_changed(sandbox, &path, &file.contents).await?;
            managed.push(path);
        }
        if managed.is_empty() {
            continue;
        }
        let mut chown = vec!["/usr/bin/chown".to_owned(), "agent:agent".to_owned()];
        chown.extend(managed);
        run_checked(sandbox, "/usr/bin/sudo", chown).await?;
    }
    write_placed(sandbox, &record, &declared).await
}

/// The skill names in a record, leaving out any that is not a single directory name, which only the
/// sandbox user can have written, so that removing a skill never reaches outside the root.
fn placed_names(contents: &[u8]) -> BTreeSet<String> {
    String::from_utf8_lossy(contents)
        .lines()
        .filter(|name| !name.is_empty() && *name != "." && *name != ".." && !name.contains('/'))
        .map(str::to_owned)
        .collect()
}

async fn write_placed(sandbox: &SandboxHandle, record: &str, names: &BTreeSet<String>) -> Result<(), Error> {
    let contents: String = names.iter().flat_map(|name| [name.as_str(), "\n"]).collect();
    if write_if_changed(sandbox, record, contents.as_bytes()).await? {
        run_checked(
            sandbox,
            "/usr/bin/sudo",
            ["/usr/bin/chown".to_owned(), "agent:agent".to_owned(), record.to_owned()],
        )
        .await?;
    }
    Ok(())
}

async fn run_checked(
    sandbox: &SandboxHandle,
    executable: &str,
    args: impl IntoIterator<Item = String>,
) -> Result<(), Error> {
    let output = sandbox
        .run_execution(ExecutionSpec::command(SandboxPath::new(executable), args))
        .await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::SandboxSetup(format!(
            "command {executable:?} exited with code {}",
            output.status.code
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::placed_names;

    #[test]
    fn a_record_names_only_single_directories() {
        let names = placed_names(b"review\n\n..\n.\nnested/skill\n../escape\nevidence\n");
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["evidence", "review"]);
    }
}
