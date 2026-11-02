//! Skills, read on the host and placed where harnesses discover them.
//!
//! A skill is a directory holding `SKILL.md`, read on the host from `spec.skills` or a folder
//! given to `agentctl skills install`. Each harness discovers skills in a directory of its own
//! below the user's home or a repository's root, which its adapter names, and this module does
//! the placement.

use std::path::Path;

use sandbox::{SandboxHandle, SandboxPath, execution::ExecutionSpec};

use crate::{Error, local::files::walk_source, sandbox::platform::files::write_if_changed};

/// One skill directory read on the host, keyed by its directory name.
#[derive(Debug, Eq, PartialEq)]
pub struct Skill {
    pub(crate) name: String,
    /// Regular files below the skill directory, as `/`-separated relative paths, sorted.
    pub(crate) files: Vec<SkillFile>,
}

/// One regular file of a skill.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct SkillFile {
    pub(crate) relative_path: String,
    pub(crate) contents: Vec<u8>,
    /// Whether it can be run, as a skill's scripts often are. Windows has no such bit, so there
    /// it is never set.
    pub(crate) executable: bool,
}

/// The file `agentctl skills install` writes into each skill it installs. Reading a skill leaves
/// it out, so a skill copied from an installed one does not carry it along.
pub(crate) const INSTALL_RECORD: &str = ".agentctl-skill.json";

impl Skill {
    /// Reads the skill in `source` to install as `name`, except an [`INSTALL_RECORD`] at its root.
    /// `field` names the source in errors.
    ///
    /// # Errors
    ///
    /// Returns an error when `name` cannot be a directory name, `source` has no `SKILL.md`, or
    /// holds a symbolic link or anything else that is not a regular file or a directory.
    pub fn read(name: String, source: &Path, field: &str) -> Result<Self, Error> {
        if !valid_name(&name) {
            return Err(Error::Invalid(format!("{name:?} is not a valid skill name")));
        }
        if !source.join("SKILL.md").is_file() {
            return Err(Error::Invalid(format!("{field} must contain SKILL.md")));
        }
        let mut files = Vec::new();
        walk_source(source, field, |relative, path, is_dir| {
            if is_dir || relative == Path::new(INSTALL_RECORD) {
                return Ok(());
            }
            let relative_path = relative
                .components()
                .map(|component| component.as_os_str().to_str())
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| Error::Invalid(format!("{field} contains a non-UTF-8 file name")))?
                .join("/");
            files.push(SkillFile {
                relative_path,
                contents: std::fs::read(path)?,
                executable: executable(&std::fs::metadata(path)?),
            });
            Ok(())
        })?;
        files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        Ok(Self { name, files })
    }

    /// The skills built into `agentctl`.
    #[must_use]
    pub fn bundled() -> Vec<Self> {
        vec![platform_skill()]
    }

    /// The directory name the skill is installed as.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(unix)]
fn executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// Whether `name` can name a skill's directory: one path component.
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\\')
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
            executable: false,
        }],
    }
}

/// Places every skill below `root` as `root/<name>/<relative path>`, owned by the image user,
/// with the executable bit each file has on the host.
///
/// Harnesses watch their skill directories live, so a file whose contents are already in place
/// is left alone rather than rewritten. Directories are created as the image user. Runtime file
/// transfer writes as the sandbox supervisor, so exactly the managed files are chowned
/// afterwards: no recursive ownership walk.
pub(super) async fn install_linux(sandbox: &SandboxHandle, root: &str, skills: &[Skill]) -> Result<(), Error> {
    for skill in skills {
        let target = format!("{root}/{}", skill.name);
        run_checked(sandbox, "/usr/bin/mkdir", ["-p".to_owned(), target.clone()]).await?;
        let mut managed = Vec::with_capacity(skill.files.len());
        let (mut executable, mut plain) = (Vec::new(), Vec::new());
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
            if file.executable { &mut executable } else { &mut plain }.push(path.clone());
            managed.push(path);
        }
        if managed.is_empty() {
            continue;
        }
        let mut chown = vec!["/usr/bin/chown".to_owned(), "agent:agent".to_owned()];
        chown.extend(managed);
        run_checked(sandbox, "/usr/bin/sudo", chown).await?;
        // Both ways, since a file left alone keeps the mode an earlier pass gave it.
        for (mode, paths) in [("a+x", executable), ("a-x", plain)] {
            if !paths.is_empty() {
                let mut chmod = vec!["/usr/bin/chmod".to_owned(), mode.to_owned(), "--".to_owned()];
                chmod.extend(paths);
                run_checked(sandbox, "/usr/bin/sudo", chmod).await?;
            }
        }
    }
    Ok(())
}

/// Removes the skill folders `names` from `root`, as the image user would. Each name is one a
/// pass placed, so a skill the image ships under another name stays.
pub(crate) async fn remove_linux(sandbox: &SandboxHandle, root: &str, names: &[&str]) -> Result<(), Error> {
    if names.is_empty() {
        return Ok(());
    }
    let mut remove = vec!["/usr/bin/rm".to_owned(), "-rf".to_owned(), "--".to_owned()];
    remove.extend(names.iter().map(|name| format!("{root}/{name}")));
    run_checked(sandbox, "/usr/bin/sudo", remove).await
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
