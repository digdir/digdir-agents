//! `agentctl skills`: skills for the harnesses on this machine, outside any agent.

use std::path::{Path, PathBuf};

use agent::{
    Harness,
    harness::Skill,
    local::{
        home::user_home_directory,
        repository_root,
        skills::{Change, Placement, apply, plan},
    },
};
use clap::{ArgGroup, Args, Subcommand};

use super::{CommandError, CommandResult, parse_harness};

#[derive(Subcommand)]
pub(crate) enum SkillsCommand {
    /// Install a skill where the harnesses on this machine discover it.
    ///
    /// The skill is copied, so later changes to its source need another install, which updates
    /// the copy. A skill of the same name that agentctl did not install, or that was edited since,
    /// is only replaced with `--force`. Without `--harness`, it is installed for each harness found
    /// on this machine: its command on `PATH`, or its own directory in your home.
    Install(Install),
}

#[derive(Args)]
#[command(group(ArgGroup::new("skill").required(true).args(["source", "bundled"])))]
#[command(group(ArgGroup::new("scope").required(true).args(["user", "repo"])))]
pub(crate) struct Install {
    /// Directory holding the skill's `SKILL.md`. The skill is installed under the name its
    /// `SKILL.md` gives, or else the directory's.
    source: Option<PathBuf>,
    /// A skill built into agentctl, such as `agentctl`, instead of a directory.
    #[arg(long, value_name = "NAME")]
    bundled: Option<String>,
    /// Install for you, in your home directory, so it is there in every repository.
    #[arg(long)]
    user: bool,
    /// Install in the repository holding DIR, such as `.`, for everyone working in it.
    #[arg(long, value_name = "DIR")]
    repo: Option<PathBuf>,
    /// Harness to install for, as a manifest names it, such as `codex`. Repeat it for several.
    #[arg(long = "harness", value_name = "HARNESS", value_parser = parse_harness)]
    harnesses: Vec<Harness>,
    /// Replace a skill of the same name that agentctl did not install, or that was edited since.
    #[arg(long)]
    force: bool,
}

pub(crate) fn execute(command: SkillsCommand) -> CommandResult<()> {
    let SkillsCommand::Install(install) = command;
    let skill = match (&install.source, &install.bundled) {
        (_, Some(name)) => bundled(name)?,
        (Some(source), None) => read(source)?,
        (None, None) => unreachable!("clap requires a source or --bundled"),
    };
    let root = match &install.repo {
        Some(directory) => {
            let directory = common::fs::canonicalize(directory)
                .map_err(|error| CommandError::Message(format!("{}: {error}", directory.display())))?;
            repository_root(&directory).map(Path::to_path_buf).ok_or_else(|| {
                CommandError::Message(format!(
                    "{} is not in a git repository other than your home directory's; --user installs it for you",
                    directory.display()
                ))
            })?
        }
        None => user_home()?,
    };
    let harnesses = if install.harnesses.is_empty() {
        found_harnesses()?
    } else {
        install.harnesses
    };

    let placements = plan(&skill, &root, &harnesses)?;
    let conflicts = placements
        .iter()
        .filter(|placement| placement.change == Change::Replace)
        .map(|placement| placement.directory.display().to_string())
        .collect::<Vec<_>>();
    if !install.force && !conflicts.is_empty() {
        return Err(CommandError::Message(format!(
            "{} already holds a skill named {} that agentctl did not install, or that was edited since; \
             pass --force to replace it",
            conflicts.join(" and "),
            skill.name()
        )));
    }
    // Each as it is done, so a failure leaves no doubt about what was already written.
    for placement in &placements {
        apply(&skill, placement).map_err(|error| {
            CommandError::Message(format!(
                "could not install {} in {}: {error}",
                skill.name(),
                placement.directory.display()
            ))
        })?;
        println!("{}", describe(skill.name(), placement));
    }
    Ok(())
}

fn bundled(name: &str) -> CommandResult<Skill> {
    let skills = Skill::bundled();
    let names = skills.iter().map(|skill| skill.name().to_owned()).collect::<Vec<_>>();
    skills.into_iter().find(|skill| skill.name() == name).ok_or_else(|| {
        CommandError::Message(format!(
            "agentctl bundles no skill named {name:?}; it bundles {}",
            names.join(", ")
        ))
    })
}

fn read(source: &Path) -> CommandResult<Skill> {
    let shown = source.display().to_string();
    let source =
        common::fs::canonicalize(source).map_err(|error| CommandError::Message(format!("{shown}: {error}")))?;
    // The Agent Skills spec names a skill's directory after the name in its frontmatter, so that
    // name wins over the one of the folder it comes from, such as a checkout's.
    let declared = std::fs::read_to_string(source.join("SKILL.md"))
        .ok()
        .and_then(|skill| declared_name(&skill));
    let name = match declared {
        Some(name) => name,
        None => source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| CommandError::Message(format!("{shown} has no directory name to install it as")))?
            .to_owned(),
    };
    // The library's message for a skill it cannot read speaks of an agent, which this is not.
    Skill::read(name, &source, &shown).map_err(|error| match error {
        agent::Error::Invalid(message) => CommandError::Message(message),
        error => error.into(),
    })
}

/// The `name` in the YAML frontmatter that opens `skill`, a `SKILL.md`. Frontmatter that YAML
/// cannot read gives none, as harnesses differ in how strictly they read it.
fn declared_name(skill: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Frontmatter {
        name: Option<String>,
    }
    let rest = skill
        .strip_prefix("---")?
        .trim_start_matches([' ', '\t', '\r'])
        .strip_prefix('\n')?;
    let end = rest.find("\n---")?;
    serde_yaml_ng::from_str::<Frontmatter>(&rest[..end]).ok()?.name
}

fn user_home() -> CommandResult<PathBuf> {
    user_home_directory()
        .ok_or_else(|| CommandError::Message("your home directory is not set (HOME or USERPROFILE)".into()))
}

fn found_harnesses() -> CommandResult<Vec<Harness>> {
    let home = user_home()?;
    let path = std::env::var_os("PATH");
    let found = Harness::ALL
        .into_iter()
        .filter(|harness| harness.found(path.as_deref(), &home))
        .collect::<Vec<_>>();
    if found.is_empty() {
        return Err(CommandError::Message(format!(
            "found no harness on this machine ({}); name harnesses with --harness",
            Harness::ALL.map(Harness::display_name).join(", ")
        )));
    }
    Ok(found)
}

fn describe(name: &str, placement: &Placement) -> String {
    let directory = placement.directory.display();
    let harnesses = display_names(&placement.harnesses);
    match placement.change {
        Change::Install => format!("installed {name} in {directory} for {harnesses}"),
        Change::Update => format!("updated {name} in {directory} for {harnesses}"),
        Change::Replace => format!("replaced {name} in {directory} for {harnesses}"),
        Change::None => format!("{name} in {directory} is already up to date for {harnesses}"),
    }
}

fn display_names(harnesses: &[Harness]) -> String {
    let names = harnesses
        .iter()
        .map(|harness| harness.display_name())
        .collect::<Vec<_>>();
    match names.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => names.concat(),
    }
}

#[cfg(test)]
mod tests {
    use super::declared_name;

    #[test]
    fn the_name_comes_from_the_frontmatter_when_it_gives_one() {
        for (skill, name) in [
            ("---\nname: review\ndescription: Reviews.\n---\nBody\n", Some("review")),
            ("---\r\nname: \"review\"\r\ndescription: x\r\n---\r\n", Some("review")),
            ("---\ndescription: No name.\n---\n", None),
            ("# No frontmatter\nname: review\n", None),
            ("---\nname: [unclosed\n---\n", None),
        ] {
            assert_eq!(declared_name(skill).as_deref(), name, "{skill:?}");
        }
    }
}
