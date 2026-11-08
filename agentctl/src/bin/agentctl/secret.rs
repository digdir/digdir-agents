//! `agentctl secret`: secrets kept on this machine under a name, which manifests ask for with
//! `secrets[].secret`, so a value that several agents use is kept once, outside their folders.

use std::fmt::Write as _;

use agent::{
    Error, names,
    secrets::{SecretReport, SecretUse, SecretsReport},
};
use clap::Subcommand;
use zeroize::Zeroizing;

use crate::{CommandResult, prompt};

#[derive(Subcommand)]
pub(crate) enum SecretCommand {
    /// Keep a secret under a name, or replace its value.
    ///
    /// At a terminal it reads the value without showing it. Agents whose manifest has
    /// `secret: NAME` on one of its `secrets` get it at once, but only at the hosts both the
    /// manifest and the secret allow.
    Set {
        /// The name manifests ask for the secret by, such as `aivar`.
        name: String,
        /// The hosts its value may reach, separated by commas, such as `api.example.com` or
        /// `*.example.com`. Needed for a new secret; a replaced one keeps its hosts without it.
        #[arg(long, value_delimiter = ',')]
        hosts: Option<Vec<String>>,
        /// Read the value from standard input instead, up to its end, without a final newline.
        #[arg(long)]
        from_stdin: bool,
    },
    /// List the kept secrets with the agents that use them, and the secrets agents ask for that
    /// are not kept.
    List,
    /// Remove a kept secret; the agents using it lose it at once.
    Remove {
        /// The secret's name, as `agentctl secret list` shows it.
        name: String,
    },
}

/// Runs `command` against agentd.
///
/// # Errors
///
/// Returns an error when the value cannot be read, agentd refuses the change, or cannot be reached.
pub(crate) async fn run(client: &agent::control_api::Client, command: SecretCommand) -> CommandResult<()> {
    match command {
        SecretCommand::Set {
            name,
            hosts,
            from_stdin,
        } => {
            let name = secret_name(&name)?;
            let value = if from_stdin {
                read_value_from_stdin()?
            } else if prompt::interactive() {
                prompt::secret(&format!("Value of the secret {name} (it is not shown)"))?
            } else {
                return Err(Error::Invalid(
                    "no terminal to type the value on; pipe it to `agentctl secret set NAME --from-stdin`".into(),
                )
                .into());
            };
            let report = client.secret_set(&name, &value, hosts).await?;
            print!("{}", kept(&report));
        }
        SecretCommand::List => print!("{}", list(&client.secrets().await?)),
        SecretCommand::Remove { name } => {
            let name = secret_name(&name)?;
            match client.secret_remove(&name).await? {
                Some(report) => print!("{}", removed(&report)),
                None => println!("No secret named {name} is kept; nothing changed."),
            }
        }
    }
    Ok(())
}

/// `value` as a secret's name, lowercased. Clap would repeat a rejected value, which may be the
/// secret's value typed in the wrong place, so names are checked here instead.
fn secret_name(value: &str) -> Result<String, Error> {
    if names::valid(value) {
        Ok(value.to_ascii_lowercase())
    } else {
        Err(Error::Invalid(names::RULE.into()))
    }
}

/// Standard input up to its end, without the newline that `echo` or a file ends with, so a value
/// of several lines, such as a key file, stays whole.
fn read_value_from_stdin() -> Result<Zeroizing<String>, Error> {
    use std::io::Read as _;

    let mut text = Zeroizing::new(String::new());
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|error| Error::Invalid(format!("could not read the value from standard input: {error}")))?;
    let value = Zeroizing::new(text.trim_end_matches(['\r', '\n']).to_owned());
    if value.is_empty() {
        return Err(Error::Invalid("no value was provided on standard input".into()));
    }
    Ok(value)
}

/// What keeping a secret did, for the agents that use it, or how a manifest uses it when none do.
fn kept(report: &SecretReport) -> String {
    let secret = &report.secret;
    let mut text = format!(
        "Kept the secret {} for {}.\n",
        secret.name,
        secret.allowed_hosts.join(", ")
    );
    if report.used_by.is_empty() {
        let _ = writeln!(
            text,
            "Agents use it with `secrets: [{{environment: VARIABLE, secret: {}, allowedHosts: [{}]}}]` in their \
             manifest.",
            secret.name,
            secret.allowed_hosts.join(", ")
        );
    } else {
        let _ = writeln!(text, "Used at once by {}.", users(&report.used_by));
    }
    text.push_str(&hosts_not_allowed(report));
    text
}

/// The kept secrets, then the ones agents ask for that are not kept, and the hosts agents send a
/// secret to that it does not allow.
fn list(report: &SecretsReport) -> String {
    let mut text = String::new();
    if report.secrets.is_empty() {
        text.push_str("No secrets are kept. Keep one with `agentctl secret set NAME --hosts HOST`.\n");
    } else {
        let rows = report
            .secrets
            .iter()
            .map(|kept| {
                vec![
                    kept.secret.name.clone(),
                    kept.secret.allowed_hosts.join(","),
                    kept.secret.updated_at.date().to_string(),
                    if kept.used_by.is_empty() {
                        "-".into()
                    } else {
                        users(&kept.used_by)
                    },
                ]
            })
            .collect::<Vec<_>>();
        for line in crate::format::table_lines(&["NAME", "HOSTS", "UPDATED", "USED BY"], &rows) {
            let _ = writeln!(text, "{line}");
        }
    }
    for missing in &report.missing {
        let (optional, required): (Vec<_>, Vec<_>) = missing.used_by.iter().partition(|used| used.optional);
        let mut effects = Vec::new();
        if !required.is_empty() {
            effects.push(format!("{} not ready", agents(&required, "is", "are")));
        }
        if !optional.is_empty() {
            effects.push(format!("{} without it", agents(&optional, "runs", "run")));
        }
        let _ = writeln!(
            text,
            "\nNo secret named {} is kept, so {}. Keep it with `agentctl secret set {0} --hosts HOST`.",
            missing.name,
            effects.join(", and ")
        );
    }
    for kept in &report.secrets {
        text.push_str(&hosts_not_allowed(kept));
    }
    text
}

/// What removing a secret did to the agents that used it.
fn removed(report: &SecretReport) -> String {
    let mut text = format!("Removed the secret {}.\n", report.secret.name);
    let (optional, required): (Vec<_>, Vec<_>) = report.used_by.iter().partition(|used| used.optional);
    if !required.is_empty() {
        let _ = writeln!(
            text,
            "{} not ready until it is kept again.",
            capitalized(&agents(&required, "is", "are"))
        );
    }
    if !optional.is_empty() {
        let _ = writeln!(
            text,
            "{} without it from now on.",
            capitalized(&agents(&optional, "runs", "run"))
        );
    }
    text
}

/// The agents in `used_by`, with the variable each holds the secret in.
fn users(used_by: &[SecretUse]) -> String {
    used_by
        .iter()
        .map(|used| format!("{} ({})", used.agent, used.environment))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The agents in `used_by`, named as a group, followed by `singular` or `plural` to match.
fn agents(used_by: &[&SecretUse], singular: &str, plural: &str) -> String {
    let mut names = used_by.iter().map(|used| used.agent.as_str()).collect::<Vec<_>>();
    names.dedup();
    match names.as_slice() {
        [one] => format!("the agent {one} {singular}"),
        many => format!("the agents {} {plural}", many.join(", ")),
    }
}

fn capitalized(text: &str) -> String {
    let mut characters = text.chars();
    characters
        .next()
        .map_or_else(String::new, |first| first.to_uppercase().chain(characters).collect())
}

/// A line for each agent that sends the secret to hosts it does not allow.
fn hosts_not_allowed(report: &SecretReport) -> String {
    let secret = &report.secret;
    let mut text = String::new();
    for used in report.used_by.iter().filter(|used| !used.hosts_not_allowed.is_empty()) {
        let mut hosts = secret.allowed_hosts.clone();
        hosts.extend(used.hosts_not_allowed.iter().cloned());
        let _ = writeln!(
            text,
            "\n{} {} to {}, which the secret does not allow, so it stays a placeholder there. \
             If it should go there, keep it again with `agentctl secret set {1} --hosts {}`.",
            capitalized(&agents(&[used], "sends", "send")),
            secret.name,
            used.hosts_not_allowed.join(", "),
            hosts.join(",")
        );
    }
    text
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use agent::secrets::{MissingSecret, Secret, SecretReport, SecretUse, SecretsReport};
    use time::macros::datetime;

    fn secret() -> Secret {
        Secret {
            name: "aivar".into(),
            allowed_hosts: vec!["api.aivar.no".into()],
            created_at: datetime!(2026-11-08 20:00 UTC),
            updated_at: datetime!(2026-11-09 08:30 UTC),
        }
    }

    fn used(agent: &str, optional: bool) -> SecretUse {
        SecretUse {
            agent: agent.into(),
            environment: "AIVAR_API_KEY".into(),
            optional,
            hosts_not_allowed: Vec::new(),
        }
    }

    #[test]
    fn keeping_a_secret_nobody_uses_shows_how_a_manifest_asks_for_it() {
        let text = super::kept(&SecretReport {
            secret: secret(),
            used_by: Vec::new(),
        });
        assert_eq!(
            text,
            "Kept the secret aivar for api.aivar.no.\n\
             Agents use it with `secrets: [{environment: VARIABLE, secret: aivar, allowedHosts: [api.aivar.no]}]` \
             in their manifest.\n"
        );
    }

    #[test]
    fn the_list_names_users_missing_secrets_and_hosts_a_secret_does_not_allow() {
        let mut wide = used("pi", false);
        wide.hosts_not_allowed = vec!["example.com".into()];
        let text = super::list(&SecretsReport {
            secrets: vec![SecretReport {
                secret: secret(),
                used_by: vec![wide],
            }],
            missing: vec![MissingSecret {
                name: "models".into(),
                used_by: vec![used("a", false), used("b", true)],
            }],
        });
        assert_eq!(
            text,
            "NAME   HOSTS         UPDATED     USED BY\n\
             aivar  api.aivar.no  2026-11-09  pi (AIVAR_API_KEY)\n\
             \n\
             No secret named models is kept, so the agent a is not ready, and the agent b runs without it. \
             Keep it with `agentctl secret set models --hosts HOST`.\n\
             \n\
             The agent pi sends aivar to example.com, which the secret does not allow, so it stays a placeholder \
             there. If it should go there, keep it again with `agentctl secret set aivar --hosts \
             api.aivar.no,example.com`.\n"
        );
    }

    #[test]
    fn removing_a_secret_says_which_agents_stop_and_which_run_without_it() {
        let text = super::removed(&SecretReport {
            secret: secret(),
            used_by: vec![used("a", false), used("b", false), used("c", true)],
        });
        assert_eq!(
            text,
            "Removed the secret aivar.\n\
             The agents a, b are not ready until it is kept again.\n\
             The agent c runs without it from now on.\n"
        );
    }
}
