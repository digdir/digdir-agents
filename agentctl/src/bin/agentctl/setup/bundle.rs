//! Copies what a manifest reads from its directory into the agent's own folder, so the folder
//! stands alone: the source it came from, such as a temporary checkout, can go.

use std::path::{Path, PathBuf};

use agent::{Agent, Error, MountSpec, manifest::resolve_host_path};
use sandbox::image::ImageSource;

/// Files never copied, wherever they sit: their values belong to the person, not the agent.
const NEVER_COPIED: &str = ".env";

/// Copies the home directory, instructions and skills `agent` reads, resolved against
/// `source_directory`, into `destination`, and points the manifest at the copies:
///
/// - the home directory to `home/`
/// - one instruction file to `instructions.md`, several to `instructions/NN-<name>` in order
/// - each skill to `skills/<name>/`, keeping its name
///
/// Optional sources that are missing are dropped. Bind mount sources and a build context are
/// made absolute, since they stay where they are. Returns the paths written, relative to
/// `destination`.
pub(super) fn bundle(agent: &mut Agent, source_directory: &Path, destination: &Path) -> Result<Vec<String>, Error> {
    let resolve = |path: &Path, field: &str| resolve_host_path(source_directory, path, field);
    let mut written = Vec::new();

    let home = resolve(&agent.spec.home.source, "spec.home.source")?;
    copy_tree(&home, &destination.join("home"))?;
    agent.spec.home.source = PathBuf::from("home");
    written.push("home/".to_owned());

    let mut instructions = Vec::new();
    for (index, spec) in agent.spec.instructions.iter().enumerate() {
        let path = resolve(&spec.source, &format!("spec.instructions[{index}].source"))?;
        if spec.optional && !path.exists() {
            continue;
        }
        instructions.push((spec.clone(), path));
    }
    let single = instructions.len() == 1;
    agent.spec.instructions = instructions
        .into_iter()
        .enumerate()
        .map(|(index, (mut spec, path))| {
            let relative = if single {
                "instructions.md".to_owned()
            } else {
                format!("instructions/{:02}-{}", index + 1, file_name(&path))
            };
            copy_file(&path, &destination.join(&relative))?;
            spec.source = PathBuf::from(&relative);
            written.push(relative);
            Ok(spec)
        })
        .collect::<Result<_, Error>>()?;

    let mut skills = Vec::new();
    for (index, mut spec) in std::mem::take(&mut agent.spec.skills).into_iter().enumerate() {
        let path = resolve(&spec.source, &format!("spec.skills[{index}].source"))?;
        if spec.optional && !path.exists() {
            continue;
        }
        let name = spec
            .name()
            .ok_or_else(|| Error::Invalid(format!("spec.skills[{index}] has no name")))?
            .to_owned();
        let relative = format!("skills/{name}");
        if written.contains(&format!("{relative}/")) {
            return Err(Error::Invalid(format!(
                "two skills are named {name:?}; give one a different `name`"
            )));
        }
        copy_tree(&path, &destination.join(&relative))?;
        spec.source = PathBuf::from(&relative);
        written.push(format!("{relative}/"));
        skills.push(spec);
    }
    agent.spec.skills = skills;

    for (index, mount) in agent.spec.sandbox.mounts.iter_mut().enumerate() {
        if let MountSpec::Bind { source, .. } = mount {
            *source = resolve(source, &format!("spec.sandbox.mounts[{index}].source"))?;
        }
    }
    if let ImageSource::Build { context, .. } = &mut agent.spec.sandbox.image {
        *context = resolve(context, "spec.sandbox.image.context")?;
    }
    Ok(written)
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || "instructions.md".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), Error> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(source, destination)
        .map_err(|error| Error::Invalid(format!("cannot copy {}: {error}", source.display())))?;
    Ok(())
}

/// Copies a directory tree, following links to their contents and leaving out `.env` files.
fn copy_tree(source: &Path, destination: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(destination)?;
    let entries = std::fs::read_dir(source)
        .map_err(|error| Error::Invalid(format!("cannot read {}: {error}", source.display())))?;
    for entry in entries {
        let entry = entry?;
        if entry.file_name().eq_ignore_ascii_case(NEVER_COPIED) {
            continue;
        }
        let path = entry.path();
        let target = destination.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            copy_file(&path, &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::path::{Path, PathBuf};

    use super::bundle;

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("directories");
        std::fs::write(path, contents).expect("a file");
    }

    /// Altinn's layout: the manifest in `agents/minimal/` reads `../home`, `../instructions.md`
    /// and `../_skills/*` beside it.
    #[test]
    fn shared_files_beside_the_manifest_folder_are_flattened_into_the_agent_folder() {
        let source = tempfile::tempdir().expect("a source");
        let root = source.path();
        write(&root.join("home/.claude/settings.json"), "{}");
        write(&root.join("home/.env"), "SECRET=1");
        write(&root.join("instructions.md"), "shared");
        write(&root.join("minimal/environment.md"), "minimal");
        write(&root.join("_skills/app-development/SKILL.md"), "app");
        write(&root.join("_skills/changelog/SKILL.md"), "changelog");
        let manifest = r"
apiVersion: agents.platform/v1alpha1
kind: Agent
metadata: {name: altinn-minimal}
spec:
  sandbox:
    image: {type: reference, reference: example.com/agent:latest}
    platform: {os: linux}
    resources: {cpu: '4', memory: 8Gi, rootFilesystem: {capacity: 64Gi, mode: layered}}
  home: {source: ../home}
  instructions:
    - source: ../instructions.md
    - source: environment.md
    - {source: missing.md, optional: true}
  skills:
    - {source: ../_skills/app-development, name: altinn-studio-app-development}
    - source: ../_skills/changelog
  harnesses: [{type: pi}]
  network: {mode: mediated, allow: all}
";
        write(&root.join("minimal/agent.yaml"), manifest);
        let mut agent = agent::manifest::resolve(&root.join("minimal/agent.yaml"))
            .expect("the manifest resolves")
            .agent;

        let destination = tempfile::tempdir().expect("a destination");
        let written = bundle(&mut agent, &root.join("minimal"), destination.path()).expect("bundled");

        assert_eq!(
            written,
            [
                "home/",
                "instructions/01-instructions.md",
                "instructions/02-environment.md",
                "skills/altinn-studio-app-development/",
                "skills/changelog/"
            ]
        );
        assert_eq!(agent.spec.home.source, PathBuf::from("home"));
        assert_eq!(agent.spec.instructions.len(), 2);
        assert_eq!(agent.spec.skills[0].name(), Some("altinn-studio-app-development"));
        assert_eq!(agent.spec.skills[1].name(), Some("changelog"));
        let out = destination.path();
        assert!(out.join("home/.claude/settings.json").is_file());
        assert!(!out.join("home/.env").exists(), ".env files are never copied");
        assert!(out.join("skills/altinn-studio-app-development/SKILL.md").is_file());

        let written_manifest = serde_yaml_ng::to_string(&agent).expect("serializes");
        std::fs::write(out.join("agent.yaml"), written_manifest).expect("written");
        agent::manifest::resolve(&out.join("agent.yaml")).expect("the bundled manifest resolves on its own");
    }
}
