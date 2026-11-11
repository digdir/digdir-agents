//! Keeping the values agents read from their environment files in agentctl's store, at the
//! terminal, and deleting the files, so that the agent builder can be given the checkout they are
//! in. The builder then changes the manifests to take the values from the store.

mod plan;

use std::{collections::BTreeSet, fmt::Write as _};

use agent::{Error, accounts::Account, control_api::Client, exposed::ExposedFile};
use plan::{Entry, Kept, MOVED_LOGIN, Plan, Store};

use crate::{CommandError, CommandResult, consent, github, prompt};

/// What `agentctl update` moves off once the person has agreed to it: the environment files in the
/// checkout, and where each of their values goes.
pub(crate) struct Moving {
    read: Vec<ExposedFile>,
    shown: Vec<String>,
    entries: Vec<Entry>,
    agents: Vec<String>,
    /// Where each value goes, as the question showed it.
    table: Vec<String>,
}

/// Asks the person to have agentctl keep the values that agents read from the environment `files`
/// in `checkout` and delete the files, so that the agent builder, which changes the manifests that
/// read them, can be given the checkout. Nothing changes until [`Moving::apply`]. Returns nothing
/// when there is no such file.
///
/// Files elsewhere, such as beside a personal agent, belong to manifests the builder is not asked
/// to change, so the person moves those off first.
///
/// # Errors
///
/// Returns what the person stores by hand when agentctl cannot store every value, such as one that
/// differs between the files, or why nothing changed when they say no.
pub(crate) async fn ask(
    client: &Client,
    files: &[ExposedFile],
    checkout: &std::path::Path,
) -> CommandResult<Option<Moving>> {
    let from = crate::builder::folder(None).ok();
    // Every environment file here, also one nothing reads any more, as a pull may have moved its
    // manifests off it while its values are in the file alone.
    let (read, outside): (Vec<_>, Vec<_>) = files
        .iter()
        .filter(|file| file.environment_file.is_some())
        .partition(|file| file.path.starts_with(checkout));
    // One elsewhere that nothing reads is only offered for deletion, as for any command.
    let outside = outside
        .into_iter()
        .filter(|file| {
            file.environment_file
                .as_ref()
                .is_some_and(|file| !file.readers.is_empty())
        })
        .collect::<Vec<_>>();
    if let Some(first) = outside.first() {
        let outside = outside.iter().copied().cloned().collect::<Vec<_>>();
        let folder = first.folder.display().to_string();
        let folder = if folder.contains(' ') {
            format!("\"{folder}\"")
        } else {
            folder
        };
        return Err(CommandError::Message(format!(
            "{}\nRun `agentctl update {folder}` first, then this again.",
            agent::exposed::refusal_from(&outside, from.as_deref()),
        )));
    }
    let mut seen = BTreeSet::new();
    let read = read
        .into_iter()
        .filter(|file| seen.insert(&file.path))
        .collect::<Vec<_>>();
    if read.is_empty() {
        return Ok(None);
    }
    let Plan {
        entries,
        others,
        agents,
    } = planned(client, &read).await?;
    let shown = read.iter().map(|file| file.shown(from.as_deref())).collect::<Vec<_>>();
    let mut lines = vec![
        "agentctl update gives the agent builder this folder, but these environment files in it hold \
         agents' keys, which an agent must not see:"
            .to_owned(),
    ];
    lines.extend(shown.iter().map(|file| format!("  {file}")));
    if !entries.is_empty() {
        lines.push("Where each value goes instead:".into());
    }
    let width = entries.iter().map(|entry| entry.what.len()).max().unwrap_or_default();
    let table = entries
        .iter()
        .map(|entry| format!("  {:width$}  {}", entry.what, entry.place))
        .collect::<Vec<_>>();
    lines.extend(table.iter().cloned());
    lines.extend(others);
    lines.push(if entries.iter().any(|entry| entry.store.is_some()) {
        "Store the keys, delete the files and go on?".into()
    } else {
        "Delete the files and go on?".into()
    });
    if !prompt::confirm_lines(&lines.iter().map(String::as_str).collect::<Vec<_>>())? {
        return Err(CommandError::Message("nothing was changed".into()));
    }
    let read = read.into_iter().cloned().collect();
    Ok(Some(Moving {
        read,
        shown,
        entries,
        agents,
        table,
    }))
}

impl Moving {
    /// Whether `file` is one of the files this moves off.
    pub(crate) fn moves(&self, file: &ExposedFile) -> bool {
        self.read.iter().any(|read| read.path == file.path)
    }

    /// Stores the values the person agreed to and deletes the files, once agentd finds nothing else
    /// reads them in `binds`. Returns what the builder's task adds: where the manifests take each
    /// value from now, which the builder cannot see.
    ///
    /// # Errors
    ///
    /// Returns an error when storing fails, or when deleting does or would lose what something has
    /// come to read since.
    pub(crate) async fn apply(
        self,
        client: &Client,
        binds: Vec<agent::control_plane::BindSource>,
    ) -> CommandResult<String> {
        let Self {
            read,
            shown,
            entries,
            agents,
            table,
        } = self;
        store(client, &entries).await?;
        // The person may have taken a while to answer, so a file something has come to read since stays.
        let now = client.exposed_files(binds).await?;
        if read.iter().any(|file| !now.contains(file)) {
            return Err(CommandError::Message(
                "the keys are stored, but what reads the files changed while you answered, so they were not deleted. \
             Run this again"
                    .into(),
            ));
        }
        let paths = read.iter().map(|file| file.path.clone()).zip(shown).collect::<Vec<_>>();
        let deleted = consent::remove(&paths)
            .map_err(|error| CommandError::Message(format!("the keys are stored, but {error}")))?;
        println!("Deleted {}.", agent::exposed::sentence_list(&deleted));
        // Also where the person sees it, in case the builder does not start.
        if !table.is_empty() {
            println!("The agent builder has the manifests take each value from here:");
            for line in &table {
                println!("{line}");
            }
        }
        if !agents.is_empty() {
            println!(
                "Apply {} again once the manifests take their values from there, so they no longer read the files.",
                agent::exposed::sentence_list(&agents)
            );
        }
        Ok(told(&entries, &agents))
    }
}

/// The [`plan::plan`] for the environment files `read`, with their values and what agentctl keeps
/// already, or what the person stores by hand first.
async fn planned(client: &Client, read: &[&ExposedFile]) -> CommandResult<Plan> {
    let mut values = Vec::new();
    for file in read {
        values.push(
            agent::environment::read(&file.path)
                .await
                .map_err(|error| CommandError::Message(format!("{}: {error}", file.path.display())))?,
        );
    }
    let files = read
        .iter()
        .filter_map(|file| file.environment_file.as_ref())
        .collect::<Vec<_>>();
    plan::plan(&files, &values, &kept(client).await?).map_err(|unkept| {
        CommandError::Message(format!(
            "these values in the environment files need storing by hand:\n  {}",
            unkept.join("\n  ")
        ))
    })
}

/// Stores what the `entries` keep. The login comes first, so the person may still decline it once
/// GitHub says whom it acts as, before anything is stored.
async fn store(client: &Client, entries: &[Entry]) -> CommandResult<()> {
    for entry in entries {
        match &entry.store {
            Some(Store::Login(token)) => {
                github::keep(client, MOVED_LOGIN, token, github::Way::Paste, None)
                    .await
                    .map_err(|error| {
                        CommandError::Message(format!(
                            "{error}. Sign in with `agentctl github login {MOVED_LOGIN}` instead, then run this again"
                        ))
                    })?;
            }
            Some(Store::Secret { name, hosts, value }) => {
                client.secret_set(name, value, Some(hosts.clone())).await?;
            }
            None => {}
        }
    }
    Ok(())
}

/// What the agent builder's task adds once the files are gone: where the manifests take each value
/// from now, and the `agents` the person applies again once the builder has changed them.
fn told(entries: &[Entry], agents: &[String]) -> String {
    let mut told = String::from("The person has deleted the environment files.");
    if !entries.is_empty() {
        // Others who pull the manifests have their values stored by the same names.
        told.push_str(
            " Have the manifests take each value from where agentctl keeps it, by exactly these names, \
             keeping each secret's allowedHosts:",
        );
    }
    told.push('\n');
    for entry in entries {
        let _ = writeln!(told, "  {}: {}", entry.what, entry.place);
    }
    if !agents.is_empty() {
        let _ = writeln!(
            told,
            "Once you have changed the manifests, tell the person to apply {} again.",
            agent::exposed::sentence_list(agents)
        );
    }
    told
}

/// What agentctl keeps already, by name.
async fn kept(client: &Client) -> Result<Kept, Error> {
    Ok(Kept {
        logins: client
            .accounts()
            .await?
            .into_iter()
            .map(|Account::Github(account)| account.name)
            .collect(),
        secrets: client
            .secrets()
            .await?
            .secrets
            .into_iter()
            .map(|report| report.secret.name)
            .collect(),
    })
}
