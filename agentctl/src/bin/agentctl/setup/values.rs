//! Values for what a manifest declares beyond GitHub: other secrets, such as an Azure DevOps
//! token or an API key, and plaintext environment. A team's agent can declare several, so the
//! person can fill them in in their editor, or be asked for each once, hidden when secret.
//! Optional ones can be left out.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
    process::Stdio,
};

use agent::{Error, control_plane::ENV_FILE};
use zeroize::Zeroizing;

use super::{
    Draft,
    answers::{Answers, Choice},
    edit, env_file,
    github::{GIT_USER_EMAIL, GIT_USER_NAME},
};

/// The sample file beside a manifest that documents its values, one comment above each.
const SAMPLE_FILE: &str = ".env.sample";

/// Fills `draft.access.values` for every declared secret and environment variable not given yet:
/// from the values known already, else from a `.env` beside a team's manifest, else from the
/// person. They fill them in in their editor when `editor` says to or they choose to, in a file in
/// `scratch` until it is read, and are otherwise asked for each in turn.
pub(super) async fn collect(answers: &Answers, scratch: &Path, editor: bool, draft: &mut Draft) -> Result<(), Error> {
    let mut known = draft.access.known_values.clone();
    let beside = agent::environment::read_or_empty(&draft.source.directory.join(ENV_FILE)).await?;
    // Values already known, such as an existing agent's own `.env`, are not asked about again.
    if !beside.is_empty()
        && draft.access.known_values.is_empty()
        && answers.confirm(
            "--env-file",
            &[&format!(
                "Use the values in {} for this agent?",
                draft.source.directory.join(ENV_FILE).display()
            )],
            true,
        )?
    {
        for (name, value) in beside {
            known.entry(name).or_insert(value);
        }
    }

    let declared = declared(draft);
    let mut missing = Vec::new();
    for declared in &declared {
        if given(draft, &declared.name) {
            continue;
        }
        match known.remove(&declared.name) {
            Some(value) if !value.is_empty() => draft.access.values.push((declared.name.clone(), value)),
            Some(_) => {}
            None => missing.push(declared),
        }
    }
    // Defaults leave an optional value out rather than ask for it.
    missing.retain(|declared| !declared.optional || (answers.interactive() && !answers.assumes_defaults()));
    let in_editor = answers.interactive()
        && !missing.is_empty()
        && (editor || (missing.len() > 1 && chooses_editor(answers, missing.len())?));
    if in_editor {
        return in_editor_until_complete(answers, scratch, &declared, draft).await;
    }
    for declared in missing {
        let default = host_default(&declared.name).await;
        let value = ask(answers, declared, default.as_deref())?;
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            draft.access.values.push((declared.name.clone(), value));
        }
    }
    Ok(())
}

/// Opens every value `draft` declares in the person's editor, with those it has filled in, in a
/// file in `scratch` until it is read.
pub(super) async fn edit(answers: &Answers, scratch: &Path, draft: &mut Draft) -> Result<(), Error> {
    let declared = declared(draft);
    if declared.is_empty() {
        eprintln!("  This agent declares no values.");
        return Ok(());
    }
    in_editor_until_complete(answers, scratch, &declared, draft).await
}

/// One value a manifest declares, with what the sample file beside it says about it.
struct Declared {
    name: String,
    secret: bool,
    optional: bool,
    hosts: String,
    hint: Option<String>,
}

/// The values `draft` declares that the person gives, which leaves out those the GitHub question
/// decided already: an optional token, and the token and identity a browser sign-in supplies once
/// the sign-in agent runs.
fn declared(draft: &Draft) -> Vec<Declared> {
    let hints = std::fs::read_to_string(draft.source.directory.join(SAMPLE_FILE))
        .map(|sample| hints(&sample))
        .unwrap_or_default();
    draft
        .agent
        .spec
        .secrets
        .iter()
        .map(|secret| (secret.source(), true, secret.optional, secret.allowed_hosts.join(", ")))
        .chain(
            draft
                .agent
                .spec
                .environment
                .iter()
                .map(|variable| (variable.source(), false, false, String::new())),
        )
        .filter(|(name, _, optional, _)| {
            let github_owned = [super::github::TOKEN_VARIABLE, GIT_USER_NAME, GIT_USER_EMAIL].contains(name);
            let token_declined = *name == super::github::TOKEN_VARIABLE && *optional;
            !token_declined && (!github_owned || draft.access.github_browser.is_none())
        })
        .map(|(name, secret, optional, hosts)| Declared {
            name: name.to_owned(),
            secret,
            optional,
            hosts,
            hint: hints.get(name).cloned(),
        })
        .collect()
}

fn given(draft: &Draft, name: &str) -> bool {
    draft.access.values.iter().any(|(given, _)| given == name)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fill {
    Editor,
    Here,
}

/// Whether the person fills in the `count` values asked for in their editor. The question is
/// asked even when defaults answer the others, since the values are asked for either way.
fn chooses_editor(answers: &Answers, count: usize) -> Result<bool, Error> {
    let question = format!("This agent needs {count} values. How do you want to fill them in?");
    let choices = vec![
        Choice::new(Fill::Editor, format!("In your editor, {}", edit::editor())),
        Choice::new(Fill::Here, "One at a time here"),
    ];
    Ok(asked_now(answers, |answers| {
        answers.choose("--edit-values", &question, choices, 0)
    })? == Fill::Editor)
}

/// Asks `question` of the person, even while defaults answer the questions around it.
fn asked_now<T>(answers: &Answers, question: impl FnOnce(&Answers) -> Result<T, Error>) -> Result<T, Error> {
    let assumed = answers.assumes_defaults();
    answers.assume_defaults(false);
    let answer = question(answers);
    answers.assume_defaults(assumed);
    answer
}

/// Removes the file of values it names once setup is done with it, however it ends.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

/// Fills `draft.access.values` with `declared` from a file the person edits, opened until every
/// required value is set or they would rather be asked for the rest here.
async fn in_editor_until_complete(
    answers: &Answers,
    scratch: &Path,
    declared: &[Declared],
    draft: &mut Draft,
) -> Result<(), Error> {
    let file = Scratch(scratch.join(format!("{}.env", draft.agent.metadata.name)));
    env_file::write_private(&file.0, &template(declared, draft).await?)?;
    loop {
        if let Err(error) = edit::open_editor(&file.0) {
            eprintln!("  {error}; setup asks for the values here instead.");
            break;
        }
        let edited = agent::environment::read_or_empty(&file.0).await?;
        let undeclared = take(declared, &edited, draft);
        if !undeclared.is_empty() {
            eprintln!(
                "  Setup leaves out {}, which this agent does not declare.",
                undeclared.join(", ")
            );
        }
        let missing = missing_required(declared, draft);
        if missing.is_empty() {
            return Ok(());
        }
        let names: Vec<&str> = missing.iter().map(|declared| declared.name.as_str()).collect();
        let question = format!("{} must be set. Edit the file again?", names.join(", "));
        let choices = vec![
            Choice::new(Fill::Editor, "Edit it again"),
            Choice::new(Fill::Here, "Fill in the rest here"),
        ];
        if asked_now(answers, |answers| {
            answers.choose("--edit-values", &question, choices, 0)
        })? == Fill::Here
        {
            break;
        }
    }
    for declared in missing_required(declared, draft) {
        let default = host_default(&declared.name).await;
        if let Some(value) = ask(answers, declared, default.as_deref())? {
            draft.access.values.push((declared.name.clone(), value));
        }
    }
    Ok(())
}

/// Takes the values of `declared` from the file the person `edited` into `draft`, an empty or
/// missing one leaving its value unset, and returns the names it has that `declared` does not.
fn take<'a>(declared: &[Declared], edited: &'a BTreeMap<String, Zeroizing<String>>, draft: &mut Draft) -> Vec<&'a str> {
    draft
        .access
        .values
        .retain(|(name, _)| !declared.iter().any(|declared| declared.name == *name));
    for declared in declared {
        if let Some(value) = edited.get(&declared.name).filter(|value| !value.is_empty()) {
            draft.access.values.push((declared.name.clone(), value.clone()));
        }
    }
    edited
        .keys()
        .map(String::as_str)
        .filter(|name| !declared.iter().any(|declared| declared.name == *name))
        .collect()
}

fn missing_required<'a>(declared: &'a [Declared], draft: &Draft) -> Vec<&'a Declared> {
    declared
        .iter()
        .filter(|declared| !declared.optional && !given(draft, &declared.name))
        .collect()
}

/// The file the person fills `declared` in in: each value with what it is above it, and the value
/// `draft` has for it, or this machine's, filled in.
async fn template(declared: &[Declared], draft: &Draft) -> Result<Zeroizing<String>, Error> {
    let mut text = Zeroizing::new(format!(
        "# Values for the agent {}. Fill in what follows each `=`, then save and close the editor.\n\
         # Secrets stay on this machine: the agent sees placeholders. Leave an optional value empty to go\n\
         # without it. Setup reads this file when the editor closes, then deletes it.\n",
        draft.agent.metadata.name
    ));
    for declared in declared {
        text.push('\n');
        if let Some(hint) = &declared.hint {
            writeln!(text, "# {hint}").ok();
        }
        let need = if declared.optional { "optional" } else { "required" };
        if declared.secret {
            writeln!(text, "# Secret, {need}. Only sent to {}.", declared.hosts).ok();
        } else {
            writeln!(text, "# Plain text, {need}. The agent sees it as it is.").ok();
        }
        let value = match draft.access.values.iter().find(|(name, _)| *name == declared.name) {
            Some((_, value)) => value.clone(),
            None => Zeroizing::new(host_default(&declared.name).await.unwrap_or_default()),
        };
        env_file::assign(&mut text, &declared.name, &value)?;
    }
    Ok(text)
}

fn ask(answers: &Answers, declared: &Declared, default: Option<&str>) -> Result<Option<Zeroizing<String>>, Error> {
    let flag = flag(&declared.name);
    let flag = flag.as_str();
    if declared.optional && (answers.assumes_defaults() || !answers.interactive()) {
        return Ok(None);
    }
    let mut lines = Vec::new();
    let skip = if declared.optional {
        " (optional, Enter to skip)"
    } else {
        ""
    };
    lines.push(format!("{}{skip}", declared.name));
    if let Some(hint) = &declared.hint {
        lines.push(hint.clone());
    }
    if declared.secret {
        lines.push(format!(
            "Only sent to {}; the agent sees a placeholder.",
            declared.hosts
        ));
    }
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    loop {
        let value = if declared.secret {
            answers.secret(flag, &lines)?
        } else {
            Zeroizing::new(answers.text(flag, &lines.join("\n"), default, |_| Ok(()))?)
        };
        if !value.is_empty() || declared.optional {
            return Ok(Some(value));
        }
        eprintln!("  {} is required.", declared.name);
    }
}

/// The flag that gives the value `name`: the Git identity has flags of its own, and any other
/// value is a line in `--env-file`.
fn flag(name: &str) -> String {
    match name {
        GIT_USER_NAME => "--git-user-name".to_owned(),
        GIT_USER_EMAIL => "--git-user-email".to_owned(),
        _ => format!("--env-file with {name}"),
    }
}

/// The value `name` has on this machine, offered as its answer: the Git identity commits here
/// carry, which a GitHub sign-in would otherwise have given.
async fn host_default(name: &str) -> Option<String> {
    let key = match name {
        GIT_USER_NAME => "user.name",
        GIT_USER_EMAIL => "user.email",
        _ => return None,
    };
    let output = tokio::process::Command::new("git")
        .args(["config", "--get", key])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (output.status.success() && !value.is_empty()).then_some(value)
}

/// The comment lines directly above each `NAME=` line in a sample file, joined.
fn hints(sample: &str) -> BTreeMap<String, String> {
    let mut hints = BTreeMap::new();
    let mut comment = Vec::new();
    for line in sample.lines().map(str::trim) {
        if let Some(text) = line.strip_prefix('#') {
            comment.push(text.trim().to_owned());
        } else if let Some((name, _)) = line.split_once('=') {
            if !comment.is_empty() {
                hints.insert(name.trim().to_owned(), comment.join(" "));
            }
            comment.clear();
        } else {
            comment.clear();
        }
    }
    hints
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use zeroize::Zeroizing;

    use super::{declared, flag, hints, missing_required, take, template};
    use crate::setup::source::team_draft;

    const MANIFEST: &str = "apiVersion: agents.platform/v1alpha1\nkind: Agent\nmetadata: {name: worker}\nspec:\n  \
                            sandbox:\n    image: {type: reference, reference: example.com/agent:latest}\n    \
                            platform: {os: linux}\n    resources: {cpu: '2', memory: 4Gi, rootFilesystem: \
                            {capacity: 20Gi, mode: layered}}\n  home: {source: home}\n  harnesses: [{type: pi}]\n  \
                            environment:\n    - {name: TEAM}\n  secrets:\n    - {environment: AZURE_DEVOPS_TOKEN, \
                            allowedHosts: [dev.azure.com]}\n    - {environment: AIVAR_API_KEY, optional: true, \
                            allowedHosts: [api.aivar.no]}\n  network: {mode: mediated, allow: all}\n";

    #[tokio::test]
    async fn values_filled_in_in_the_editor_come_back_as_given() {
        let folder = tempfile::tempdir().expect("a folder");
        std::fs::create_dir_all(folder.path().join("home")).expect("a home");
        std::fs::write(folder.path().join("agent.yaml"), MANIFEST).expect("a manifest");
        std::fs::write(
            folder.path().join(".env.sample"),
            "# The team's Azure DevOps token.\nAZURE_DEVOPS_TOKEN=\n",
        )
        .expect("a sample");
        let save_as = "mine".parse().expect("a variant name");
        let mut draft = team_draft(&folder.path().join("agent.yaml"), "a test", &save_as).expect("a draft");
        draft
            .access
            .values
            .push(("TEAM".to_owned(), Zeroizing::new("Platform team".to_owned())));
        let declared = declared(&draft);

        let text = template(&declared, &draft).await.expect("a template");
        assert!(text.contains("# The team's Azure DevOps token.\n# Secret, required. Only sent to dev.azure.com.\n"));
        assert!(text.contains("# Secret, optional. Only sent to api.aivar.no.\nAIVAR_API_KEY=\n"));
        assert!(text.contains("TEAM=\"Platform team\"\n"), "{}", text.as_str());

        // The person fills in the token, leaves the optional key empty and adds a name by mistake.
        let edited = text.replace(
            "AZURE_DEVOPS_TOKEN=\n",
            "AZURE_DEVOPS_TOKEN=secret token\nAZURE_TOKEN=x\n",
        );
        let path = folder.path().join("worker.env");
        std::fs::write(&path, &edited).expect("edited");
        let read = agent::environment::read_or_empty(&path).await.expect("read back");
        assert_eq!(take(&declared, &read, &mut draft), ["AZURE_TOKEN"]);
        let values: Vec<(&str, &str)> = draft
            .access
            .values
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        assert_eq!(
            values,
            [("AZURE_DEVOPS_TOKEN", "secret token"), ("TEAM", "Platform team")]
        );
        assert!(missing_required(&declared, &draft).is_empty());

        // Emptying a required value leaves it to be asked for.
        std::fs::write(&path, &text).expect("emptied");
        let read = agent::environment::read_or_empty(&path).await.expect("read back");
        take(&declared, &read, &mut draft);
        let missing: Vec<&str> = missing_required(&declared, &draft)
            .iter()
            .map(|declared| declared.name.as_str())
            .collect();
        assert_eq!(missing, ["AZURE_DEVOPS_TOKEN"]);
    }

    #[test]
    fn the_git_identity_is_given_by_its_own_flags() {
        assert_eq!(flag("GIT_USER_NAME"), "--git-user-name");
        assert_eq!(flag("GIT_USER_EMAIL"), "--git-user-email");
        assert_eq!(flag("AIVAR_API_KEY"), "--env-file with AIVAR_API_KEY");
    }

    #[test]
    fn a_sample_files_comments_explain_the_value_below_them() {
        let sample = "# Selected Git identity values enter the Sandbox in plaintext.\nGIT_USER_NAME=Your Name\n\
                      GITHUB_TOKEN=\n# Optional: the aivar gateway key for Pi,\n# mediated to api.aivar.no only.\n\
                      AIVAR_API_KEY=\n";
        let hints = hints(sample);
        assert_eq!(
            hints.get("GIT_USER_NAME").map(String::as_str),
            Some("Selected Git identity values enter the Sandbox in plaintext.")
        );
        assert!(!hints.contains_key("GITHUB_TOKEN"));
        assert_eq!(
            hints.get("AIVAR_API_KEY").map(String::as_str),
            Some("Optional: the aivar gateway key for Pi, mediated to api.aivar.no only.")
        );
    }
}
