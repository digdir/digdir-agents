//! The questions that shape an agent once its source is known: its name, which may replace a
//! definition already in ~/agents, the size of its machine, and its default harness.

use super::answers::{Answers, Choice};
use super::sizing::{Host, presets};
use super::{AGENTS_FOLDER, Draft, SetupArguments, SizeChoice, existing};
use crate::{CommandError, CommandResult};
use agent::{Agent, Error, Harness};
use std::path::Path;

/// The questions that shape an agent once its source is known: name, size and default harness.
pub(super) struct Shape<'a> {
    pub(super) arguments: &'a SetupArguments,
    pub(super) answers: &'a Answers,
    pub(super) storage: &'a Path,
    pub(super) names: &'a Names<'a>,
}

impl Shape<'_> {
    pub(super) fn apply(&self, draft: &mut Draft) -> CommandResult<()> {
        choose_name(self.arguments, self.answers, draft, self.names)?;
        choose_size(self.arguments, self.answers, draft, self.storage)?;
        choose_harness(self.arguments, self.answers, draft)
    }
}

/// The names agents can have: those of agentd's agents and the folders in ~/agents are taken,
/// except a folder setup may replace.
pub(super) struct Names<'a> {
    pub(super) agents: &'a [Agent],
    pub(super) root: &'a Path,
    pub(super) found: &'a [existing::Existing],
    /// The definition the person already chose to replace.
    pub(super) target: Option<&'a existing::Existing>,
    /// Whether a definition in ~/agents may be replaced at all; not in a dry run.
    pub(super) allow_replace: bool,
}

impl Names<'_> {
    fn free(&self, name: &str) -> bool {
        !self.agents.iter().any(|agent| agent.metadata.name == name) && !self.root.join(name).exists()
    }

    /// The definition in ~/agents/<name> that a new agent of that name would replace.
    fn replaceable(&self, name: &str) -> Option<&existing::Existing> {
        if !self.allow_replace {
            return None;
        }
        let folder = self.root.join(name);
        self.found
            .iter()
            .find(|existing| existing.folder == folder)
            .filter(|existing| existing.state != existing::State::NameUsedElsewhere)
            // An agent of this name from another folder keeps the name taken.
            .filter(|existing| {
                existing.agent_here() == Some(name) || !self.agents.iter().any(|agent| agent.metadata.name == name)
            })
    }

    /// Why `name` cannot be used, unless by replacing what is there.
    fn check(&self, name: &str) -> Result<(), String> {
        sandbox::SandboxName::new(name.to_owned()).map_err(|error| format!("{name:?}: {error}"))?;
        if self.free(name) || self.replaceable(name).is_some() {
            Ok(())
        } else if self.root.join(name).exists() && self.allow_replace {
            Err(format!(
                "~/{AGENTS_FOLDER}/{name} holds no agent definition setup can replace; choose another name"
            ))
        } else {
            Err(format!(
                "{name:?} is taken by an agent or a folder in ~/{AGENTS_FOLDER}"
            ))
        }
    }
}

pub(super) fn choose_name(
    arguments: &SetupArguments,
    answers: &Answers,
    draft: &mut Draft,
    names: &Names<'_>,
) -> CommandResult<()> {
    let check = |name: &str| names.check(name);
    let mut offered = names.target.map_or_else(
        || crate::names::free_name(&draft.agent.metadata.name, |name| !names.free(name)),
        |target| target.name.clone(),
    );
    loop {
        let name = if let Some(name) = &arguments.name {
            check(name).map_err(Error::Invalid)?;
            name.clone()
        } else {
            answers.text("--name", "Agent name", Some(&offered), check)?
        };
        let Some(replaced) = names.replaceable(&name).filter(|_| !names.free(&name)) else {
            draft.agent.metadata.name = name;
            draft.replace = None;
            return Ok(());
        };
        let confirmed = names.target == Some(replaced) || arguments.replace || {
            if !answers.interactive() {
                return Err(CommandError::Message(format!(
                    "~/{AGENTS_FOLDER}/{name} already holds an agent definition; pass --replace to replace it, \
                     or another --name"
                )));
            }
            let mut what = format!("~/{AGENTS_FOLDER}/{name} already holds an agent definition");
            if let Some(agent) = replaced.agent_here() {
                what = format!("{what}, and agent/{agent} runs from it");
            }
            answers.choose(
                "--replace",
                &format!("{what}. Replace it?"),
                vec![
                    Choice::new(false, "No, choose another name"),
                    Choice::new(true, "Yes: remove the folder, and delete the agent with its disk")
                        .flag("--replace", &[]),
                ],
                0,
            )?
        };
        if confirmed {
            draft.agent.metadata.name = name;
            draft.replace = Some(replaced.clone());
            return Ok(());
        }
        if arguments.name.is_some() {
            return Err(CommandError::Message(format!("{name} is taken; choose another --name")));
        }
        offered = crate::names::free_name(&name, |name| !names.free(name));
    }
}

pub(super) fn choose_size(
    arguments: &SetupArguments,
    answers: &Answers,
    draft: &mut Draft,
    storage: &Path,
) -> CommandResult<()> {
    let host = Host::measure(storage);
    if let Some(shortfall) = host.shortfall(&draft.minimum) {
        eprintln!(
            "warning: {shortfall}; this agent needs at least {}",
            draft.minimum.describe()
        );
    }
    let [small, medium, large] = presets(&host, draft.minimum, draft.recommended);
    let size = match arguments.size {
        Some(SizeChoice::S) => small,
        Some(SizeChoice::M) => medium,
        Some(SizeChoice::L) => large,
        None => answers.choose(
            "--size",
            "How large should the agent's machine be?",
            vec![
                Choice::new(small, format!("Small: {}", small.describe())).flag("--size", &["s"]),
                Choice::new(medium, format!("Medium: {}", medium.describe()))
                    .suggested()
                    .flag("--size", &["m"]),
                Choice::new(large, format!("Large: {}", large.describe())).flag("--size", &["l"]),
            ],
            1,
        )?,
    };
    size.apply(&mut draft.agent.spec.sandbox.resources)?;
    Ok(())
}

pub(super) fn choose_harness(arguments: &SetupArguments, answers: &Answers, draft: &mut Draft) -> CommandResult<()> {
    let harnesses = &mut draft.agent.spec.harnesses;
    let declared: Vec<Harness> = harnesses.iter().map(|harness| harness.kind).collect();
    let current = harnesses.iter().position(|harness| harness.default).unwrap_or_default();
    let chosen = match arguments.harness {
        Some(harness) if declared.contains(&harness) => harness,
        Some(harness) => {
            let declared: Vec<_> = declared.iter().map(|harness| harness.as_str()).collect();
            return Err(CommandError::Message(format!(
                "this agent has no {harness} harness; it has {}",
                declared.join(", ")
            )));
        }
        None => answers.choose(
            "--harness",
            "Which harness should sessions use unless they choose another?",
            declared
                .iter()
                .map(|harness| Choice::new(*harness, harness.display_name()).flag("--harness", &[harness.as_str()]))
                .collect(),
            current,
        )?,
    };
    if harnesses.len() > 1 {
        for harness in harnesses.iter_mut() {
            harness.default = harness.kind == chosen;
            // The default is always installed, so its login is never skipped.
            harness.optional &= harness.kind != chosen;
        }
    }
    Ok(())
}
