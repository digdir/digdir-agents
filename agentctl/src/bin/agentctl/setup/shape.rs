//! The questions that shape an agent once its source is known: its name, the size of its machine,
//! its harnesses and the default one. Changing an agent the person has, they offer its own
//! answers, and its name stays.

use super::answers::{Answers, Choice};
use super::sizing::{Host, Whole, presets};
use super::{Draft, SetupArguments, SizeChoice};
use crate::{CommandError, CommandResult};
use agent::{Agent, Error, Harness};
use std::path::Path;

/// The questions that shape an agent once its source is known: name, size and harnesses.
pub(super) struct Shape<'a> {
    pub(super) arguments: &'a SetupArguments,
    pub(super) answers: &'a Answers,
    pub(super) storage: &'a Path,
    pub(super) names: &'a Names<'a>,
}

impl Shape<'_> {
    pub(super) fn apply(&self, draft: &mut Draft) -> CommandResult<()> {
        if !draft.changing {
            choose_name(self.arguments, self.answers, draft, self.names)?;
        }
        choose_size(self.arguments, self.answers, draft, self.storage)?;
        choose_harnesses(self.arguments, self.answers, draft)
    }
}

/// The names agents can have: those of agentd's agents are taken.
pub(super) struct Names<'a> {
    pub(super) agents: &'a [Agent],
}

impl Names<'_> {
    /// Why `name` cannot be used for an agent.
    fn check(&self, name: &str) -> Result<(), String> {
        sandbox::SandboxName::new(name.to_owned()).map_err(|error| format!("{name:?}: {error}"))?;
        if self.agents.iter().any(|agent| agent.metadata.name == name) {
            Err(format!("{name:?} is taken by another agent"))
        } else {
            Ok(())
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
    let name = if let Some(name) = &arguments.name {
        check(name).map_err(Error::Invalid)?;
        name.clone()
    } else {
        let offered = crate::names::free_name(&draft.agent.metadata.name, |name| check(name).is_err());
        answers.text("--name", "Agent name", Some(&offered), check)?
    };
    draft.agent.metadata.name = name;
    Ok(())
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
    let current = Whole::of_resources(&draft.agent.spec.sandbox.resources);
    // A sandbox's disk can only grow, so an agent the person has is offered none smaller.
    let mut minimum = draft.minimum;
    if draft.changing {
        minimum.disk_gib = minimum.disk_gib.max(current.disk_gib);
    }
    let [small, medium, large] = presets(&host, minimum, draft.recommended);
    let mut choices = vec![
        Choice::new(Sized::Preset(small), format!("Small: {}", small.describe())).flag("--size", &["s"]),
        Choice::new(Sized::Preset(medium), format!("Medium: {}", medium.describe())).flag("--size", &["m"]),
        Choice::new(Sized::Preset(large), format!("Large: {}", large.describe())).flag("--size", &["l"]),
        Choice::new(Sized::Custom, "Your own: choose the CPUs, memory and disk"),
    ];
    // An agent the person has keeps its size unless they choose another.
    let offered = if draft.changing {
        choices.insert(
            0,
            Choice::new(Sized::Preset(current), format!("As it is: {}", current.describe())).suggested(),
        );
        current
    } else {
        choices[1] = choices.remove(1).suggested();
        medium
    };
    let own = [arguments.cpus, arguments.memory_gib, arguments.disk_gib];
    let size = match arguments.size {
        Some(SizeChoice::S) => small,
        Some(SizeChoice::M) => medium,
        Some(SizeChoice::L) => large,
        None if own.iter().any(Option::is_some) => own_size(arguments, answers, &host, minimum, offered)?,
        None => {
            let default = usize::from(!draft.changing);
            match answers.choose("--size", "How large should the agent's machine be?", choices, default)? {
                Sized::Preset(size) => size,
                Sized::Custom => own_size(arguments, answers, &host, minimum, offered)?,
            }
        }
    };
    size.apply(&mut draft.agent.spec.sandbox.resources)?;
    Ok(())
}

/// A size offered for the agent's machine, or one the person chooses part by part.
#[derive(Clone, Copy)]
enum Sized {
    Preset(Whole),
    Custom,
}

/// A size of the person's own: each part from its flag, else asked for, offering `offered`'s. A
/// part is never below `minimum`, which holds an existing agent's disk, nor more than `host` has.
fn own_size(
    arguments: &SetupArguments,
    answers: &Answers,
    host: &Host,
    minimum: Whole,
    offered: Whole,
) -> CommandResult<Whole> {
    let parts = [
        Part {
            flag: "--cpus",
            question: "How many CPUs?",
            unit: "CPUs",
            available: "CPUs",
            given: arguments.cpus,
            offered: offered.cpus,
            minimum: minimum.cpus,
            most: Some(host.cpus),
        },
        Part {
            flag: "--memory-gib",
            question: "How much memory, in GiB?",
            unit: "GiB memory",
            available: "GiB memory",
            given: arguments.memory_gib,
            offered: offered.memory_gib,
            minimum: minimum.memory_gib,
            most: host.memory_gib,
        },
        Part {
            flag: "--disk-gib",
            question: "How much disk, in GiB?",
            unit: "GiB disk",
            available: "GiB free disk",
            given: arguments.disk_gib,
            offered: offered.disk_gib,
            minimum: minimum.disk_gib,
            most: host.free_disk_gib,
        },
    ];
    let [cpus, memory_gib, disk_gib] = parts.map(|part| part.choose(answers));
    Ok(Whole {
        cpus: cpus?,
        memory_gib: memory_gib?,
        disk_gib: disk_gib?,
    })
}

/// One part of a size of the person's own, such as its memory.
struct Part {
    flag: &'static str,
    question: &'static str,
    unit: &'static str,
    /// What the host's measure counts, such as the free part of its disk.
    available: &'static str,
    given: Option<u64>,
    offered: u64,
    minimum: u64,
    /// What the host has, when it can be measured.
    most: Option<u64>,
}

impl Part {
    fn choose(&self, answers: &Answers) -> CommandResult<u64> {
        if let Some(given) = self.given {
            self.check(&given.to_string())
                .map_err(|reason| Error::Invalid(format!("{} {given}: {reason}", self.flag)))?;
            return Ok(given);
        }
        // What is offered fits, unless the host is smaller than the agent needs.
        let offered = self
            .most
            .map_or(self.offered, |most| self.offered.min(most))
            .max(self.minimum.max(1));
        let answer = answers.text(self.flag, self.question, Some(&offered.to_string()), |answer| {
            self.check(answer)
        })?;
        Ok(answer
            .trim()
            .parse()
            .map_err(|_| Error::Invalid(format!("{answer:?} is not a number")))?)
    }

    /// Why `answer` cannot be this part, if it cannot. A host smaller than the agent needs was
    /// warned about already, so only the agent's minimum holds then.
    fn check(&self, answer: &str) -> Result<(), String> {
        let value: u64 = answer.trim().parse().map_err(|_| "Type a whole number.".to_owned())?;
        let minimum = self.minimum.max(1);
        if value < minimum {
            return Err(format!("This agent needs at least {minimum} {}.", self.unit));
        }
        if let Some(most) = self.most.filter(|most| *most >= minimum && value > *most) {
            return Err(format!("This machine has {most} {}.", self.available));
        }
        Ok(())
    }
}

pub(super) fn choose_harnesses(arguments: &SetupArguments, answers: &Answers, draft: &mut Draft) -> CommandResult<()> {
    let declared: Vec<Harness> = draft.base.spec.harnesses.iter().map(|harness| harness.kind).collect();
    let named = arguments.harnesses.iter().chain(&arguments.harness);
    if let Some(harness) = named.into_iter().find(|harness| !declared.contains(harness)) {
        let declared: Vec<_> = declared.iter().map(|harness| harness.as_str()).collect();
        return Err(CommandError::Message(format!(
            "this agent has no {harness} harness; it has {}",
            declared.join(", ")
        )));
    }
    let kept: Vec<Harness> = if arguments.harnesses.is_empty() {
        let has: Vec<usize> = declared
            .iter()
            .enumerate()
            .filter(|(_, kind)| draft.agent.spec.harnesses.iter().any(|harness| harness.kind == **kind))
            .map(|(index, _)| index)
            .collect();
        answers.choose_many(
            "--harnesses",
            "Which harnesses should the agent have?",
            declared
                .iter()
                .map(|harness| Choice::new(*harness, harness.display_name()).flag("--harnesses", &[harness.as_str()]))
                .collect(),
            &has,
        )?
    } else {
        declared
            .iter()
            .copied()
            .filter(|harness| arguments.harnesses.contains(harness))
            .collect()
    };
    if let Some(harness) = arguments.harness.filter(|harness| !kept.contains(harness)) {
        return Err(CommandError::Message(format!(
            "--harness {harness} must be one of the --harnesses the agent has"
        )));
    }
    // Each harness kept as the person has it, or as the manifest declares one they add back.
    let mut harnesses: Vec<_> = draft
        .base
        .spec
        .harnesses
        .iter()
        .filter(|harness| kept.contains(&harness.kind))
        .map(|declared| {
            draft
                .agent
                .spec
                .harnesses
                .iter()
                .find(|harness| harness.kind == declared.kind)
                .unwrap_or(declared)
                .clone()
        })
        .collect();
    let current = harnesses.iter().position(|harness| harness.default).unwrap_or_default();
    let chosen = match arguments.harness {
        Some(harness) => harness,
        None => answers.choose(
            "--harness",
            "Which harness should new sessions use by default?",
            kept.iter()
                .map(|harness| Choice::new(*harness, harness.display_name()).flag("--harness", &[harness.as_str()]))
                .collect(),
            current,
        )?,
    };
    for harness in &mut harnesses {
        harness.default = harness.kind == chosen;
        // The default is always installed, so its login is never skipped.
        harness.optional &= harness.kind != chosen;
    }
    draft.agent.spec.harnesses = harnesses;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use clap::{Args, FromArgMatches};

    use super::{choose_size, own_size};
    use crate::setup::{
        SetupArguments,
        answers::Answers,
        sizing::{Host, Whole},
        source::team_draft,
    };

    const HOST: Host = Host {
        cpus: 8,
        memory_gib: Some(16),
        free_disk_gib: Some(100),
    };
    const MINIMUM: Whole = Whole {
        cpus: 2,
        memory_gib: 4,
        disk_gib: 20,
    };
    const OFFERED: Whole = Whole {
        cpus: 4,
        memory_gib: 8,
        disk_gib: 40,
    };

    fn sized(flags: &[&str]) -> Result<Whole, String> {
        let command = SetupArguments::augment_args(clap::Command::new("setup"));
        let matches = command
            .try_get_matches_from(std::iter::once("setup").chain(flags.iter().copied()))
            .expect("valid flags");
        let arguments = SetupArguments::from_arg_matches(&matches).expect("arguments");
        own_size(&arguments, &Answers::new(false, true), &HOST, MINIMUM, OFFERED).map_err(|error| error.to_string())
    }

    #[test]
    fn a_size_of_ones_own_takes_the_parts_given_and_offers_the_rest() {
        assert_eq!(
            sized(&["--cpus", "6", "--disk-gib", "60"]),
            Ok(Whole {
                cpus: 6,
                memory_gib: 8,
                disk_gib: 60
            })
        );
        assert_eq!(sized(&[]), Ok(OFFERED));
    }

    #[test]
    fn a_size_of_ones_own_is_within_the_agents_minimum_and_the_machine() {
        let below = sized(&["--memory-gib", "2"]).expect_err("below the minimum");
        assert!(below.contains("at least 4 GiB memory"), "{below}");
        let above = sized(&["--cpus", "12"]).expect_err("more than the machine has");
        assert!(above.contains("This machine has 8 CPUs"), "{above}");
        let disk = sized(&["--disk-gib", "200"]).expect_err("more than is free");
        assert!(disk.contains("100 GiB free disk"), "{disk}");
    }

    #[test]
    fn a_size_of_ones_own_replaces_the_presets() {
        let command = SetupArguments::augment_args(clap::Command::new("setup"));
        assert!(
            command
                .try_get_matches_from(["setup", "--size", "m", "--cpus", "2"])
                .is_err()
        );
    }

    #[test]
    fn an_agent_the_person_has_is_offered_no_smaller_disk() {
        let folder = tempfile::tempdir().expect("a folder");
        std::fs::create_dir_all(folder.path().join("home")).expect("a home");
        std::fs::write(
            folder.path().join("agent.yaml"),
            "apiVersion: agents.platform/v1alpha1\nkind: Agent\nmetadata: {name: worker}\nspec:\n  sandbox:\n    \
             image: {type: reference, reference: example.com/agent:latest}\n    platform: {os: linux}\n    \
             resources: {cpu: '2', memory: 4Gi, rootFilesystem: {capacity: 204800Mi, mode: layered}}\n  \
             home: {source: home}\n  harnesses: [{type: pi}]\n  network: {mode: mediated, allow: all}\n",
        )
        .expect("a manifest");
        let save_as = "mine".parse().expect("a variant name");
        let mut draft = team_draft(&folder.path().join("agent.yaml"), "a test", &save_as).expect("a draft");
        draft.changing = true;
        let command = SetupArguments::augment_args(clap::Command::new("setup"));
        let matches = command
            .try_get_matches_from(["setup", "--size", "s"])
            .expect("valid flags");
        let arguments = SetupArguments::from_arg_matches(&matches).expect("arguments");

        choose_size(&arguments, &Answers::new(false, true), &mut draft, folder.path()).expect("a size");
        assert_eq!(Whole::of_resources(&draft.agent.spec.sandbox.resources).disk_gib, 200);
    }
}
