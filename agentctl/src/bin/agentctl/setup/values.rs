//! Values for what a manifest declares beyond GitHub: other secrets, such as an Azure DevOps
//! token or an API key, and plaintext environment. A team's agent can declare several; each is
//! asked for once, hidden when secret, and optional ones can be left out.

use std::{collections::BTreeMap, path::Path};

use agent::{Error, control_plane::ENV_FILE};
use zeroize::Zeroizing;

use super::{Draft, answers::Answers};

/// The sample file beside a manifest that documents its values, one comment above each.
const SAMPLE_FILE: &str = ".env.sample";

/// Fills `draft.access.values` for every declared secret and environment variable not given yet: from
/// `env_file`, else from a `.env` beside a team's manifest, else by asking.
pub(super) async fn collect(answers: &Answers, env_file: Option<&Path>, draft: &mut Draft) -> Result<(), Error> {
    let mut known = match env_file {
        Some(path) => agent::environment::read_or_empty(path).await?,
        None => BTreeMap::new(),
    };
    for (name, value) in &draft.access.known_values {
        known.entry(name.clone()).or_insert_with(|| value.clone());
    }
    let beside = agent::environment::read_or_empty(&draft.source.directory.join(ENV_FILE)).await?;
    let hints = std::fs::read_to_string(draft.source.directory.join(SAMPLE_FILE))
        .map(|sample| hints(&sample))
        .unwrap_or_default();
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

    let declared: Vec<Declared> = draft
        .agent
        .spec
        .secrets
        .iter()
        .map(|secret| Declared {
            name: secret.source().to_owned(),
            secret: true,
            optional: secret.optional,
            hosts: secret.allowed_hosts.join(", "),
        })
        .chain(draft.agent.spec.environment.iter().map(|variable| Declared {
            name: variable.source().to_owned(),
            secret: false,
            optional: false,
            hosts: String::new(),
        }))
        .collect();
    for declared in declared {
        if draft.access.values.iter().any(|(name, _)| *name == declared.name) {
            continue;
        }
        // The GitHub question already decided an optional token, and the browser sign-in supplies
        // the token and the identity once the sign-in agent runs.
        let github_owned = [
            super::github::TOKEN_VARIABLE,
            super::github::GIT_USER_NAME,
            super::github::GIT_USER_EMAIL,
        ]
        .contains(&declared.name.as_str());
        let token_declined = declared.name == super::github::TOKEN_VARIABLE && declared.optional;
        if token_declined || (github_owned && draft.access.github_browser.is_some()) {
            continue;
        }
        let value = match known.remove(&declared.name) {
            Some(value) => Some(value),
            None => ask(answers, &declared, hints.get(&declared.name).map(String::as_str))?,
        };
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            draft.access.values.push((declared.name, value));
        }
    }
    Ok(())
}

struct Declared {
    name: String,
    secret: bool,
    optional: bool,
    hosts: String,
}

fn ask(answers: &Answers, declared: &Declared, hint: Option<&str>) -> Result<Option<Zeroizing<String>>, Error> {
    let flag = format!("--env-file with {}", declared.name);
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
    if let Some(hint) = hint {
        lines.push(hint.to_owned());
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
            Zeroizing::new(answers.text(flag, lines[0], None, |_| Ok(()))?)
        };
        if !value.is_empty() || declared.optional {
            return Ok(Some(value));
        }
        eprintln!("  {} is required.", declared.name);
    }
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
    use super::hints;

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
