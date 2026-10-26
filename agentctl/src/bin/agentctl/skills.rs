//! `agentctl skills`: skills for the harnesses on this machine, outside any Agent.

use std::path::{Path, PathBuf};

use agent::{
    Harness,
    harness::Skill,
    local::{
        home::user_home_directory,
        skills::{Change, Placement, apply, plan, repository_root},
    },
};
use clap::{ArgGroup, Args, Subcommand};

use super::{CommandError, CommandResult, parse_harness};

#[derive(Subcommand)]
pub(crate) enum SkillsCommand {
    /// Install a skill where the harnesses on this machine discover it.
    ///
    /// The skill is copied, so later changes to its source need another install. Without
    /// `--harness`, it is installed for each harness found on this machine: its command on
    /// `PATH`, or its own directory in your home.
    Install(Install),
}

#[derive(Args)]
#[command(group(ArgGroup::new("skill").required(true).args(["source", "bundled"])))]
#[command(group(ArgGroup::new("scope").required(true).args(["user", "repo"])))]
pub(crate) struct Install {
    /// Directory holding the skill's `SKILL.md`.
    source: Option<PathBuf>,
    /// A skill built into agentctl, such as `agentctl`, instead of a directory.
    #[arg(long, value_name = "NAME", conflicts_with = "name")]
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
    /// Name to install the skill as, instead of its directory's name.
    #[arg(long)]
    name: Option<String>,
    /// Replace a different skill of the same name.
    #[arg(long)]
    force: bool,
}

pub(crate) fn execute(command: SkillsCommand) -> CommandResult<()> {
    let SkillsCommand::Install(install) = command;
    let skill = match (&install.source, &install.bundled) {
        (_, Some(name)) => bundled(name)?,
        (Some(source), None) => read(source, install.name)?,
        (None, None) => unreachable!("clap requires a source or --bundled"),
    };
    let root = match &install.repo {
        Some(directory) => {
            let directory = std::fs::canonicalize(directory)
                .map_err(|error| CommandError::Message(format!("{}: {error}", directory.display())))?;
            repository_root(&directory)
                .ok_or_else(|| CommandError::Message(format!("{} is not in a git repository", directory.display())))?
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
            "a different skill named {} is already in {}; pass --force to replace it",
            skill.name(),
            conflicts.join(" and ")
        )));
    }
    apply(&skill, &placements)?;
    for placement in &placements {
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

fn read(source: &Path, name: Option<String>) -> CommandResult<Skill> {
    let shown = source.display().to_string();
    let source = std::fs::canonicalize(source).map_err(|error| CommandError::Message(format!("{shown}: {error}")))?;
    let name = match name {
        Some(name) => name,
        None => source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| CommandError::Message(format!("{shown} has no directory name to install it as")))?
            .to_owned(),
    };
    // The library's message for a skill it cannot read speaks of an Agent, which this is not.
    Skill::read(name, &source, &shown).map_err(|error| match error {
        agent::Error::Invalid(message) => CommandError::Message(message),
        error => error.into(),
    })
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
