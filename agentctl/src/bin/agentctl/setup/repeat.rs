//! The command that sets up the same agent again without questions: the flags setup was given,
//! with those its answers stand for in place of the questions.

use std::path::Path;

use clap::Args as _;

use super::{
    SetupArguments,
    answers::{Answers, Flag},
};

/// Flags the repeated command leaves out: it passes `--non-interactive` and `--yes` itself, and
/// an edit in an editor is not something a flag repeats.
const LEFT_OUT: &[&str] = &["--yes", "--non-interactive", "--edit", "--dry-run"];

/// Whether `flag` with `values` belongs in a command run without a terminal. Signing in again
/// needs one, and the logins it made are held by then, so the repeat uses those.
fn repeatable(flag: &str, values: &[String]) -> bool {
    let signs_in_again = flag == "--sign-in" && values == ["again"];
    !LEFT_OUT.contains(&flag) && !signs_in_again
}

/// The command, run with `cli`, that repeats a setup given the words `given` after `setup` and
/// answered as `answers` recorded, reading the values it asked for from `env_file`.
pub(super) fn command(cli: &str, given: &[String], answers: &Answers, env_file: Option<&Path>) -> String {
    let mut flags = given_flags(given);
    for (flag, values) in answers.flags() {
        set(&mut flags, flag, values);
    }
    if let Some(env_file) = env_file {
        set(
            &mut flags,
            "--env-file".to_owned(),
            Some(vec![env_file.display().to_string()]),
        );
    }
    let mut words = vec![
        cli.to_owned(),
        "setup".to_owned(),
        "--non-interactive".to_owned(),
        "--yes".to_owned(),
    ];
    for (flag, values) in flags {
        let Some(values) = values.filter(|values| repeatable(&flag, values)) else {
            continue;
        };
        if values.is_empty() {
            words.push(flag);
            continue;
        }
        for value in values {
            words.push(flag.clone());
            words.push(super::shell_word(&value));
        }
    }
    words.join(" ")
}

/// The words given after `setup` on this command line, any that is not Unicode with its invalid
/// bytes replaced.
pub(super) fn given() -> Vec<String> {
    std::env::args_os()
        .map(|word| word.to_string_lossy().into_owned())
        .skip_while(|word| word != "setup")
        .skip(1)
        .collect()
}

fn set(flags: &mut Vec<Flag>, flag: String, values: Option<Vec<String>>) {
    flags.retain(|(set, _)| *set != flag);
    flags.push((flag, values));
}

/// The flags among `given` by their long names, the values of a repeated one together.
fn given_flags(given: &[String]) -> Vec<Flag> {
    let command = SetupArguments::augment_args(clap::Command::new("setup"));
    let mut flags: Vec<Flag> = Vec::new();
    let mut words = given.iter();
    while let Some(word) = words.next() {
        let (name, inline) = word
            .split_once('=')
            .map_or((word.as_str(), None), |(name, value)| (name, Some(value.to_owned())));
        let argument = argument(&command, name);
        let Some(long) = argument.and_then(clap::Arg::get_long) else {
            continue;
        };
        let takes_value = argument.is_some_and(|argument| argument.get_action().takes_values());
        let value = if takes_value {
            inline.or_else(|| words.next().cloned())
        } else {
            None
        };
        let flag = format!("--{long}");
        if let Some((_, Some(values))) = flags.iter_mut().find(|(known, _)| *known == flag) {
            values.extend(value);
        } else {
            flags.push((flag, Some(value.into_iter().collect())));
        }
    }
    flags
}

/// The argument `name`, a long flag such as `--size` or a short one such as `-y`, stands for.
fn argument<'a>(command: &'a clap::Command, name: &str) -> Option<&'a clap::Arg> {
    if let Some(long) = name.strip_prefix("--") {
        return command
            .get_arguments()
            .find(|argument| argument.get_long() == Some(long));
    }
    let mut short = name.strip_prefix('-')?.chars();
    let (Some(short), None) = (short.next(), short.next()) else {
        return None;
    };
    command
        .get_arguments()
        .find(|argument| argument.get_short() == Some(short))
}

#[cfg(test)]
mod tests {
    use super::command;
    use crate::setup::answers::Answers;

    #[test]
    fn the_answers_join_the_flags_given_and_replace_those_they_answer_again() {
        let answers = Answers::new(false, true);
        answers.record("--size", &["m"]);
        answers.record("--github", &["existing"]);
        answers.record("--github-user", &["bdl-agent"]);
        answers.forget("--github-token-stdin");
        answers.record("--sign-in", &["again"]);
        let given: Vec<String> = [
            "--home",
            "x",
            "-y",
            "--github=token",
            "--github-token-stdin",
            "--name",
            "api",
        ]
        .iter()
        .map(|&word| word.to_owned())
        .collect();
        let command = command(
            "agentctl",
            &given,
            &answers,
            Some(std::path::Path::new("/agents/api/.env")),
        );
        assert_eq!(
            command,
            "agentctl setup --non-interactive --yes --name api --size m --github existing --github-user bdl-agent \
             --env-file /agents/api/.env"
        );
    }
}
