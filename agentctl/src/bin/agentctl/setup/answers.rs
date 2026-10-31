//! Where each setup answer comes from: its flag, else a question at the terminal, else its
//! default with `--yes`, or while setup fills in the defaults to show them for review. Without a
//! terminal, a missing answer fails and names its flag, so a script learns exactly what to pass.
//!
//! Each answer also records the flag that gives it, so setup can say how to repeat the same setup
//! without questions. Going back to the question before starts setup again, replaying every answer
//! given until then, so each step runs as it does the first time. Changing something in the review
//! starts it again the same way, asking each question that defaults answered.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
};

use agent::Error;
use zeroize::Zeroizing;

use crate::prompt::{self, Reply};

pub(super) struct Answers {
    interactive: bool,
    /// Whether `--yes` was given, so defaults answer what no flag does throughout.
    yes: bool,
    /// Whether defaults answer what no flag does: always with `--yes`, and while setup fills them
    /// in to show for review.
    assume_defaults: Cell<bool>,
    /// Answers given before going back, given again without asking.
    replay: RefCell<VecDeque<Asked>>,
    /// Answers given at the terminal, in order.
    asked: RefCell<Vec<Asked>>,
    /// The flags that give the answers so far.
    flags: RefCell<Vec<Flag>>,
    /// Whether a question may still go back, which ends once setup has done something.
    may_go_back: Cell<bool>,
    went_back: Cell<bool>,
    /// Whether the person asked to change what defaults answered, which ends this run.
    starting_over: Cell<bool>,
    cancelled: Cell<bool>,
}

/// A flag and its values, or `None` when the command must leave it out.
pub(super) type Flag = (String, Option<Vec<String>>);

/// The answers given before going back, minus the last.
pub(super) struct Replay(Vec<Asked>);

#[derive(Clone)]
struct Asked {
    /// The question, by its flag and wording.
    key: String,
    given: Given,
}

#[derive(Clone)]
enum Given {
    Index(usize),
    Indices(Vec<usize>),
    Text(String),
    Bool(bool),
    Secret(Zeroizing<String>),
}

/// One choice offered for a question, with the label people see and the flags that make it.
pub(super) struct Choice<T> {
    pub(super) value: T,
    pub(super) label: String,
    flags: Vec<(String, Vec<String>)>,
}

impl<T> Choice<T> {
    pub(super) fn new(value: T, label: impl Into<String>) -> Self {
        Self {
            value,
            label: label.into(),
            flags: Vec::new(),
        }
    }

    /// Marks this as the choice to make for someone who does not know which to make.
    pub(super) fn suggested(mut self) -> Self {
        self.label.push_str(" (if unsure, select this)");
        self
    }

    /// Adds `flag` with `values`, which a setup without questions passes to make this choice.
    pub(super) fn flag(mut self, flag: &str, values: &[&str]) -> Self {
        self.flags
            .push((flag.to_owned(), values.iter().map(|&value| value.to_owned()).collect()));
        self
    }
}

impl Answers {
    pub(super) const fn new(interactive: bool, assume_defaults: bool) -> Self {
        Self {
            interactive,
            yes: assume_defaults,
            assume_defaults: Cell::new(assume_defaults),
            replay: RefCell::new(VecDeque::new()),
            asked: RefCell::new(Vec::new()),
            flags: RefCell::new(Vec::new()),
            may_go_back: Cell::new(true),
            went_back: Cell::new(false),
            starting_over: Cell::new(false),
            cancelled: Cell::new(false),
        }
    }

    /// Gives the answers in `replay` again, in order, while the questions are the same.
    pub(super) fn replaying(self, replay: Replay) -> Self {
        *self.replay.borrow_mut() = replay.0.into();
        self
    }

    /// Whether questions can be asked, so that what only a person can do, such as a sign-in in a
    /// browser, is offered.
    pub(super) const fn interactive(&self) -> bool {
        self.interactive
    }

    /// Whether defaults answer what no flag does.
    pub(super) const fn assumes_defaults(&self) -> bool {
        self.assume_defaults.get()
    }

    /// Whether defaults are answering only to be shown for review, so what they decide need not be
    /// reported as it happens.
    pub(super) const fn filling_in(&self) -> bool {
        self.assume_defaults.get() && !self.yes
    }

    /// Lets defaults answer what no flag does from here on, or stops them. Answers given this way
    /// are not remembered, so starting over asks those questions.
    pub(super) fn assume_defaults(&self, assume: bool) {
        self.assume_defaults.set(assume);
    }

    /// Ends this run so that setup starts again, asking each question defaults answered.
    pub(super) fn start_over(&self) {
        self.starting_over.set(true);
    }

    /// Whether the person asked to change what defaults answered.
    pub(super) const fn starting_over(&self) -> bool {
        self.starting_over.get()
    }

    /// Whether a person answered any question.
    pub(super) fn asked_anything(&self) -> bool {
        !self.asked.borrow().is_empty()
    }

    /// Whether the person went back, which ended this run of setup with an error.
    pub(super) const fn went_back(&self) -> bool {
        self.went_back.get()
    }

    /// Whether the person cancelled a question, which ended this run of setup with an error.
    pub(super) const fn cancelled(&self) -> bool {
        self.cancelled.get()
    }

    /// The answers to give again after going back: all but the last, which is asked again.
    pub(super) fn rewind(self) -> Replay {
        let mut asked = self.asked.into_inner();
        asked.pop();
        Replay(asked)
    }

    /// Ends going back, once setup does something that answering again would do twice.
    pub(super) fn no_way_back(&self) {
        self.may_go_back.set(false);
    }

    /// Records that `flag` with `values` gives an answer, in place of what it recorded before.
    pub(super) fn record(&self, flag: &str, values: &[&str]) {
        self.set_flag(flag, Some(values.iter().map(|&value| value.to_owned()).collect()));
    }

    /// Records that `flag` must be left out, as when another answer replaces it.
    pub(super) fn forget(&self, flag: &str) {
        self.set_flag(flag, None);
    }

    /// The flags recorded, in the order of their answers.
    pub(super) fn flags(&self) -> Vec<Flag> {
        self.flags.borrow().clone()
    }

    fn set_flag(&self, flag: &str, values: Option<Vec<String>>) {
        let mut flags = self.flags.borrow_mut();
        flags.retain(|(recorded, _)| recorded != flag);
        flags.push((flag.to_owned(), values));
    }

    /// Picks one of `choices`. `default` is the index taken with `--yes` and offered first. A
    /// single choice is taken without asking.
    pub(super) fn choose<T>(
        &self,
        flag: &str,
        question: &str,
        choices: Vec<Choice<T>>,
        default: usize,
    ) -> Result<T, Error> {
        self.pick(flag, question, choices, default, false)
    }

    /// Picks one of `choices` as [`Self::choose`] does, but shows a single choice too, for a
    /// choice people should see what they get from.
    pub(super) fn choose_shown<T>(
        &self,
        flag: &str,
        question: &str,
        choices: Vec<Choice<T>>,
        default: usize,
    ) -> Result<T, Error> {
        self.pick(flag, question, choices, default, true)
    }

    fn pick<T>(
        &self,
        flag: &str,
        question: &str,
        mut choices: Vec<Choice<T>>,
        default: usize,
        show_single: bool,
    ) -> Result<T, Error> {
        if choices.is_empty() {
            return Err(Error::Invalid(format!("nothing to choose for {question}")));
        }
        let default = default.min(choices.len() - 1);
        let single = choices.len() == 1 && !(show_single && self.interactive);
        let index = if self.assumes_defaults() || single {
            default
        } else if self.interactive {
            let key = key(flag, question);
            let index = match self.replayed(&key) {
                Some(Given::Index(index)) if index < choices.len() => index,
                _ => {
                    let labels: Vec<String> = choices.iter().map(|choice| choice.label.clone()).collect();
                    match prompt::menu(question, &labels, default, self.can_go_back())? {
                        Reply::Answer(index) => index,
                        Reply::Back => return Err(self.back()),
                        Reply::Cancel => return Err(self.cancel()),
                    }
                }
            };
            self.remember(key, Given::Index(index));
            index
        } else {
            return Err(missing(flag, Some(&choices[default].label)));
        };
        let choice = choices.swap_remove(index);
        for (flag, values) in &choice.flags {
            let values: Vec<&str> = values.iter().map(String::as_str).collect();
            self.record(flag, &values);
        }
        Ok(choice.value)
    }

    /// Picks one or more of `choices`. `defaults` are the indexes taken with `--yes` and offered
    /// chosen. A single choice is taken without asking. The flags of the choices picked are
    /// recorded together, unless the picks are the defaults, which `--yes` repeats.
    pub(super) fn choose_many<T>(
        &self,
        flag: &str,
        question: &str,
        choices: Vec<Choice<T>>,
        defaults: &[usize],
    ) -> Result<Vec<T>, Error> {
        if choices.is_empty() {
            return Err(Error::Invalid(format!("nothing to choose for {question}")));
        }
        let defaults: Vec<usize> = defaults
            .iter()
            .copied()
            .filter(|index| *index < choices.len())
            .collect();
        let picked = if choices.len() == 1 {
            vec![0]
        } else if self.assumes_defaults() {
            defaults.clone()
        } else if self.interactive {
            let key = key(flag, question);
            let picked = match self.replayed(&key) {
                Some(Given::Indices(picked)) if picked.iter().all(|index| *index < choices.len()) => picked,
                _ => {
                    let labels: Vec<String> = choices.iter().map(|choice| choice.label.clone()).collect();
                    match prompt::multi_menu(question, &labels, &defaults, self.can_go_back())? {
                        Reply::Answer(picked) => picked,
                        Reply::Back => return Err(self.back()),
                        Reply::Cancel => return Err(self.cancel()),
                    }
                }
            };
            self.remember(key, Given::Indices(picked.clone()));
            picked
        } else {
            let labels: Vec<&str> = defaults.iter().map(|index| choices[*index].label.as_str()).collect();
            return Err(missing(flag, Some(&labels.join(", "))));
        };
        if picked != defaults {
            let mut flags: Vec<(String, Vec<String>)> = Vec::new();
            for (flag, values) in picked.iter().flat_map(|index| &choices[*index].flags) {
                match flags.iter_mut().find(|(known, _)| known == flag) {
                    Some((_, all)) => all.extend(values.iter().cloned()),
                    None => flags.push((flag.clone(), values.clone())),
                }
            }
            for (flag, values) in &flags {
                let values: Vec<&str> = values.iter().map(String::as_str).collect();
                self.record(flag, &values);
            }
        }
        Ok(choices
            .into_iter()
            .enumerate()
            .filter(|(index, _)| picked.contains(index))
            .map(|(_, choice)| choice.value)
            .collect())
    }

    /// Asks for text that `check` accepts, offering `default`. A non-empty answer to a plain flag
    /// is recorded as that flag.
    pub(super) fn text(
        &self,
        flag: &str,
        question: &str,
        default: Option<&str>,
        check: impl Fn(&str) -> Result<(), String>,
    ) -> Result<String, Error> {
        let answer = self.typed(flag, question, default, check)?;
        if flag.starts_with("--") && !flag.contains(' ') && !answer.trim().is_empty() {
            self.record(flag, &[answer.trim()]);
        }
        Ok(answer)
    }

    fn typed(
        &self,
        flag: &str,
        question: &str,
        default: Option<&str>,
        check: impl Fn(&str) -> Result<(), String>,
    ) -> Result<String, Error> {
        if self.assumes_defaults()
            && let Some(default) = default
        {
            check(default).map_err(Error::Invalid)?;
            return Ok(default.to_owned());
        }
        if !self.interactive {
            return Err(missing(flag, default));
        }
        let key = key(flag, question);
        if let Some(Given::Text(answer)) = self.replayed(&key)
            && check(&answer).is_ok()
        {
            self.remember(key, Given::Text(answer.clone()));
            return Ok(answer);
        }
        let lines: Vec<&str> = question.lines().collect();
        loop {
            match prompt::line(&lines, default, self.can_go_back())? {
                Reply::Answer(answer) => match check(&answer) {
                    Ok(()) => {
                        self.remember(key, Given::Text(answer.clone()));
                        return Ok(answer);
                    }
                    Err(reason) => eprintln!("  {reason}"),
                },
                Reply::Back => return Err(self.back()),
                Reply::Cancel => return Err(self.cancel()),
            }
        }
    }

    /// Asks a yes-or-no question; `--yes` answers `default`.
    pub(super) fn confirm(&self, flag: &str, lines: &[&str], default: bool) -> Result<bool, Error> {
        if self.assumes_defaults() {
            return Ok(default);
        }
        if !self.interactive {
            return Err(missing(flag, Some(if default { "yes" } else { "no" })));
        }
        let key = key(flag, &lines.join("\n"));
        let answer = match self.replayed(&key) {
            Some(Given::Bool(answer)) => answer,
            _ => match prompt::yes_no(lines, self.can_go_back())? {
                Reply::Answer(answer) => answer,
                Reply::Back => return Err(self.back()),
                Reply::Cancel => return Err(self.cancel()),
            },
        };
        self.remember(key, Given::Bool(answer));
        Ok(answer)
    }

    /// Asks for a secret without showing it. There is no default and `--yes` cannot answer it.
    pub(super) fn secret(&self, flag: &str, lines: &[&str]) -> Result<Zeroizing<String>, Error> {
        if !self.interactive {
            return Err(missing(flag, None));
        }
        let key = key(flag, &lines.join("\n"));
        let secret = match self.replayed(&key) {
            Some(Given::Secret(secret)) => secret,
            _ => match prompt::secret(lines, self.can_go_back())? {
                Reply::Answer(secret) => secret,
                Reply::Back => return Err(self.back()),
                Reply::Cancel => return Err(self.cancel()),
            },
        };
        self.remember(key, Given::Secret(secret.clone()));
        Ok(secret)
    }

    /// Whether going back from this question leads anywhere: setup has done nothing yet, and an
    /// answer before it can be given again.
    fn can_go_back(&self) -> bool {
        self.may_go_back.get() && !self.asked.borrow().is_empty()
    }

    /// The answer given to the question `key` before going back, while the questions are the
    /// same as then. Once they differ, nothing more is replayed.
    fn replayed(&self, key: &str) -> Option<Given> {
        let mut replay = self.replay.borrow_mut();
        if replay.front().is_some_and(|asked| asked.key == key) {
            replay.pop_front().map(|asked| asked.given)
        } else {
            replay.clear();
            None
        }
    }

    fn remember(&self, key: String, given: Given) {
        self.asked.borrow_mut().push(Asked { key, given });
    }

    /// Marks this run as cancelled, with the error that ends it.
    fn cancel(&self) -> Error {
        self.cancelled.set(true);
        Error::Invalid("cancelled".into())
    }

    /// Marks this run as ended by going back, with the error that ends it.
    fn back(&self) -> Error {
        self.went_back.set(true);
        Error::Invalid("went back to the question before".into())
    }
}

fn key(flag: &str, question: &str) -> String {
    format!("{flag}\n{question}")
}

fn missing(flag: &str, default: Option<&str>) -> Error {
    Error::Invalid(default.map_or_else(
        || format!("{flag} is needed without a terminal"),
        |default| format!("{flag} is needed without a terminal; pass it, or --yes to accept {default:?}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::{Answers, Asked, Choice, Given, Replay, key};

    fn harnesses() -> Vec<Choice<&'static str>> {
        ["claudeCode", "codex", "pi"]
            .into_iter()
            .map(|harness| Choice::new(harness, harness).flag("--harnesses", &[harness]))
            .collect()
    }

    #[test]
    fn defaults_filled_in_are_not_remembered_so_starting_over_asks_them() {
        assert!(!Answers::new(true, true).filling_in(), "--yes reports what it decides");
        let answers = Answers::new(true, false);
        answers.assume_defaults(true);
        assert!(answers.filling_in());
        let choices = vec![Choice::new(1, "one"), Choice::new(2, "two")];
        assert_eq!(answers.choose("--size", "Size?", choices, 1).ok(), Some(2));
        assert!(!answers.asked_anything());
        answers.assume_defaults(false);
        assert!(!answers.assumes_defaults());
        answers.start_over();
        assert!(answers.starting_over());
    }

    #[test]
    fn picking_several_records_their_flags_together_unless_they_are_the_defaults() {
        let question = "Which harnesses?";
        let defaults = [0, 1, 2];
        let yes = Answers::new(false, true);
        let all = yes.choose_many("--harnesses", question, harnesses(), &defaults).ok();
        assert_eq!(all, Some(vec!["claudeCode", "codex", "pi"]));
        assert!(yes.flags().is_empty(), "--yes repeats the defaults");

        let without_terminal = Answers::new(false, false);
        let error = without_terminal
            .choose_many("--harnesses", question, harnesses(), &defaults)
            .err()
            .map(|error| error.to_string());
        assert!(error.is_some_and(|error| error.contains("--harnesses")));

        let given = Asked {
            key: key("--harnesses", question),
            given: Given::Indices(vec![0, 2]),
        };
        let replayed = Answers::new(true, false).replaying(Replay(vec![given]));
        let picked = replayed
            .choose_many("--harnesses", question, harnesses(), &defaults)
            .ok();
        assert_eq!(picked, Some(vec!["claudeCode", "pi"]));
        assert_eq!(
            replayed.flags(),
            [(
                "--harnesses".to_owned(),
                Some(vec!["claudeCode".to_owned(), "pi".to_owned()])
            )]
        );
    }

    #[test]
    fn without_a_terminal_a_missing_answer_names_its_flag() {
        let answers = Answers::new(false, false);
        let choices = vec![Choice::new(1, "one"), Choice::new(2, "two")];
        let error = answers
            .choose("--size", "Size?", choices, 1)
            .err()
            .map(|error| error.to_string());
        assert!(error.is_some_and(|error| error.contains("--size") && error.contains("two")));
        assert!(answers.text("--name", "Name?", None, |_| Ok(())).is_err());
    }

    #[test]
    fn yes_accepts_defaults_but_still_checks_them() {
        let answers = Answers::new(false, true);
        let choices = vec![Choice::new(1, "one"), Choice::new(2, "two")];
        assert_eq!(answers.choose("--size", "Size?", choices, 1).ok(), Some(2));
        assert_eq!(
            answers
                .text("--name", "Name?", Some("worker"), |_| Ok(()))
                .ok()
                .as_deref(),
            Some("worker")
        );
        assert!(
            answers
                .text("--name", "Name?", Some("Bad"), |_| Err("no".into()))
                .is_err()
        );
        assert!(answers.secret("--github-token-stdin", &["Token"]).is_err());
    }

    #[test]
    fn a_choice_records_its_flags_and_a_later_one_replaces_them() {
        let answers = Answers::new(false, true);
        let choices = vec![
            Choice::new(1, "one").flag("--size", &["s"]),
            Choice::new(2, "two").flag("--size", &["m"]),
        ];
        answers.choose("--size", "Size?", choices, 1).ok();
        answers.text("--name", "Name?", Some("worker"), |_| Ok(())).ok();
        answers.record("--size", &["l"]);
        answers.forget("--github-token-stdin");
        assert_eq!(
            answers.flags(),
            [
                ("--name".to_owned(), Some(vec!["worker".to_owned()])),
                ("--size".to_owned(), Some(vec!["l".to_owned()])),
                ("--github-token-stdin".to_owned(), None),
            ]
        );
    }
}
