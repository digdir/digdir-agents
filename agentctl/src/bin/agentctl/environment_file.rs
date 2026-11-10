//! Keeping the values agents read from their environment files in agentctl's store, at the
//! terminal, and deleting the files, so that the agent builder can be given the checkout they are
//! in. The builder then changes the manifests to take the values from the store.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

use agent::{
    Error,
    accounts::Account,
    control_api::Client,
    exposed::{EnvironmentFile, ExposedFile, MOVED_LOGIN, Move, Reader, Variable},
};
use zeroize::Zeroizing;

use crate::{CommandError, CommandResult, consent, github, prompt};

/// The values of each environment file, by variable.
type Values = [BTreeMap<String, Zeroizing<String>>];

/// Where a value of the environment files goes, and what storing it there takes.
struct Entry {
    /// The variables agents get it as.
    what: String,
    /// Where the manifests take it from once the files are gone.
    place: String,
    /// What agentctl stores, unless it is stored already or a plain value, which the builder writes
    /// into the manifests.
    store: Option<Store>,
}

enum Store {
    /// The GitHub token, kept as the login [`MOVED_LOGIN`].
    Login(Zeroizing<String>),
    Secret {
        name: String,
        hosts: Vec<String>,
        value: Zeroizing<String>,
    },
}

/// Offers to keep the values that agents read from the environment `files` in `checkout` in
/// agentctl's store, then deletes the files, so that `binds` can be given to the agent builder,
/// which changes the manifests that read them. Returns, once the files are gone, what the builder's
/// task adds: where the manifests take each value from now, which the builder cannot see. Returns
/// nothing when no agent reads any of those files.
///
/// Files elsewhere, such as beside a personal agent, belong to manifests the builder is not asked
/// to change, so they are left to the refusal.
///
/// # Errors
///
/// Returns what the person stores by hand when agentctl cannot store every value, such as one that
/// differs between the files, or why nothing changed when they say no, or when storing or deleting
/// fails.
pub(crate) async fn move_off(
    client: &Client,
    files: &[ExposedFile],
    checkout: &std::path::Path,
    binds: Vec<agent::control_plane::BindSource>,
) -> CommandResult<Option<String>> {
    let mut seen = BTreeSet::new();
    let read = files
        .iter()
        .filter(|file| {
            file.path.starts_with(checkout)
                && file
                    .environment_file
                    .as_ref()
                    .is_some_and(|file| !file.readers.is_empty())
        })
        .filter(|file| seen.insert(&file.path))
        .collect::<Vec<_>>();
    if read.is_empty() {
        return Ok(None);
    }
    let from = crate::builder::folder(None).ok();
    let environment = || read.iter().filter_map(|file| file.environment_file.as_ref());
    let moving = EnvironmentFile::moving(environment());
    let mut values = Vec::new();
    for file in &read {
        values.push(agent::environment::read(&file.path).await?);
    }
    let entries = entries(&agent::exposed::moves(&moving), &values, &stored(client).await?).map_err(|unkept| {
        CommandError::Message(format!(
            "these values in the environment files need storing by hand:\n  {}",
            unkept.join("\n  ")
        ))
    })?;
    let shown = read.iter().map(|file| file.shown(from.as_deref())).collect::<Vec<_>>();
    let mut lines = vec![
        "agentctl update gives the agent builder this checkout, but agents read their keys from these \
         environment files in it, which an agent must not see:"
            .to_owned(),
    ];
    lines.extend(shown.iter().map(|file| format!("  {file}")));
    lines.push("Where each value goes instead:".into());
    let width = entries.iter().map(|entry| entry.what.len()).max().unwrap_or_default();
    let table = entries
        .iter()
        .map(|entry| format!("  {:width$}  {}", entry.what, entry.place))
        .collect::<Vec<_>>();
    lines.extend(table.iter().cloned());
    lines.extend(unread(environment(), &moving, &values));
    lines.push(if entries.iter().any(|entry| entry.store.is_some()) {
        "Store the keys, delete the files and go on?".into()
    } else {
        "Delete the files and go on?".into()
    });
    if !prompt::confirm_lines(&lines.iter().map(String::as_str).collect::<Vec<_>>())? {
        return Err(CommandError::Message("nothing was changed".into()));
    }
    // The login comes first, so the person may still decline it once GitHub says whom it acts as
    // before anything is stored.
    for entry in &entries {
        match &entry.store {
            Some(Store::Login(token)) => {
                github::keep(client, MOVED_LOGIN, token, github::Way::Paste, None).await?;
            }
            Some(Store::Secret { name, hosts, value }) => {
                client.secret_set(name, value, Some(hosts.clone())).await?;
            }
            None => {}
        }
    }
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
    let deleted =
        consent::remove(&paths).map_err(|error| CommandError::Message(format!("the keys are stored, but {error}")))?;
    // Also where the person sees it, in case the builder does not start.
    println!(
        "Deleted {}. The agent builder has the manifests take each value from here:",
        agent::exposed::sentence_list(&deleted)
    );
    for line in &table {
        println!("{line}");
    }
    let mut told = String::from(
        "The person has deleted the environment files, and agentctl keeps their values instead. Have the \
                    manifests take them from there:\n",
    );
    for entry in &entries {
        let _ = writeln!(told, "  {}: {}", entry.what, entry.place);
    }
    let agents = EnvironmentFile::agents(environment());
    if !agents.is_empty() {
        let _ = writeln!(
            told,
            "Once you have changed the manifests, tell the person to apply {} again.",
            agent::exposed::sentence_list(&agents)
        );
    }
    Ok(Some(told))
}

/// What a manifest beside the `files` that agentctl cannot read may lose with them: every variable
/// in the files, given their `values`, that is not `moving`. Nothing tells what such a manifest
/// reads, but it can read only what the files hold.
fn unread<'a>(
    files: impl IntoIterator<Item = &'a EnvironmentFile>,
    moving: &[Variable],
    values: &Values,
) -> Option<String> {
    let manifests = files
        .into_iter()
        .flat_map(|file| &file.readers)
        .filter_map(|reader| match reader {
            Reader::Unreadable { name, .. } => Some(name.clone()),
            Reader::Manifest { .. } | Reader::Agent { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    if manifests.is_empty() {
        return None;
    }
    let these = agent::exposed::sentence_list(&manifests.into_iter().collect::<Vec<_>>());
    let read = moving
        .iter()
        .map(|variable| variable.source.as_deref().unwrap_or(&variable.name))
        .collect::<BTreeSet<_>>();
    let others = values
        .iter()
        .flat_map(BTreeMap::keys)
        .filter(|name| !read.contains(name.as_str()))
        .cloned()
        .collect::<BTreeSet<_>>();
    Some(if others.is_empty() {
        format!("agentctl cannot read {these}, but the files hold nothing else they could read.")
    } else {
        format!(
            "agentctl cannot read {these}, so they may also read {}, which go with the files.",
            agent::exposed::sentence_list(&others.into_iter().collect::<Vec<_>>())
        )
    })
}

/// The names of the GitHub logins and the secrets agentctl keeps already.
struct Stored {
    logins: BTreeSet<String>,
    secrets: BTreeSet<String>,
}

async fn stored(client: &Client) -> Result<Stored, Error> {
    Ok(Stored {
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

/// Where each of `moves` goes, in their order, given the `values` of each file and what is
/// `stored` already, or what agentctl cannot store, each with what the person does instead. A value
/// no file holds is left out, as nothing that reads it could have started.
fn entries(moves: &[Move], values: &Values, stored: &Stored) -> Result<Vec<Entry>, Vec<String>> {
    // A plain value is shown and given to the builder, so one that a manifest reads as a secret is not.
    let secret = moves
        .iter()
        .filter(|change| !matches!(change, Move::Plain { .. }))
        .map(Move::source)
        .collect::<BTreeSet<_>>();
    let mut entries = Vec::new();
    let mut unkept = Vec::new();
    for change in moves {
        let source = change.source();
        let what = change
            .variables()
            .iter()
            .map(|variable| variable.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        // Where it goes, whether it is there already, and how the person puts it there.
        let (place, there, by_hand) = match change {
            Move::GithubLogin { .. } => (
                format!("the GitHub login {MOVED_LOGIN}"),
                stored.logins.contains(MOVED_LOGIN),
                format!("Sign in with `agentctl github login {MOVED_LOGIN}`"),
            ),
            Move::Secret { name, hosts, .. } => {
                let command = format!(
                    "`agentctl secret set {} --hosts {}`",
                    name.as_deref().unwrap_or("NAME"),
                    if hosts.is_empty() {
                        "HOST".to_owned()
                    } else {
                        hosts.join(",")
                    }
                );
                let Some(name) = name.as_deref().filter(|_| !hosts.is_empty()) else {
                    unkept.push(format!(
                        "{source}: agentctl cannot tell what to store it as. Store it with {command}, then run this again"
                    ));
                    continue;
                };
                (
                    format!("the secret {name}, for {}", hosts.join(", ")),
                    stored.secrets.contains(name),
                    format!("Store the one agents should get with {command}"),
                )
            }
            Move::Plain { .. } if secret.contains(source) => {
                unkept.push(format!(
                    "{source}: one manifest reads it as a plain value and another as a secret. Make it a secret in \
                     both, then run this again"
                ));
                continue;
            }
            Move::Plain { .. } => (String::new(), false, "Make them agree".to_owned()),
        };
        if there {
            entries.push(Entry {
                what,
                place: format!("{place}, stored already"),
                store: None,
            });
            continue;
        }
        // The files are one place for a value, so they must agree on it.
        let mut found = values.iter().filter_map(|values| values.get(source));
        let value = found.next();
        if found.any(|other| Some(other) != value) {
            unkept.push(format!(
                "{source}: the files hold different values. {by_hand}, then run this again"
            ));
            continue;
        }
        let Some(value) = value.cloned() else {
            continue;
        };
        let (place, store) = match change {
            Move::GithubLogin { .. } => (place, Some(Store::Login(value))),
            Move::Secret {
                name: Some(name),
                hosts,
                ..
            } => (
                place,
                Some(Store::Secret {
                    name: name.clone(),
                    hosts: hosts.clone(),
                    value,
                }),
            ),
            Move::Secret { name: None, .. } | Move::Plain { .. } => (
                format!(
                    "the value {}, which the agent builder writes into the manifests",
                    value.as_str()
                ),
                None,
            ),
        };
        entries.push(Entry { what, place, store });
    }
    if unkept.is_empty() { Ok(entries) } else { Err(unkept) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variable(name: &str, hosts: Option<&[&str]>) -> Variable {
        Variable {
            name: name.into(),
            source: None,
            allowed_hosts: hosts.map(|hosts| hosts.iter().map(|&host| host.to_owned()).collect()),
        }
    }

    const NOTHING: Stored = Stored {
        logins: BTreeSet::new(),
        secrets: BTreeSet::new(),
    };

    fn file(values: &[(&str, &str)]) -> BTreeMap<String, Zeroizing<String>> {
        values
            .iter()
            .map(|&(name, value)| (name.to_owned(), Zeroizing::new(value.to_owned())))
            .collect()
    }

    fn places(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| format!("{}: {}", entry.what, entry.place))
            .collect()
    }

    #[test]
    fn the_values_of_the_files_are_kept_where_the_manifests_will_take_them() {
        let moves = agent::exposed::moves(&[
            variable("GITHUB_TOKEN", Some(&["github.com"])),
            variable("GIT_USER_NAME", None),
            variable("AZURE_DEVOPS_PAT", Some(&["dev.azure.com"])),
            variable("STUDIO_API_KEY", Some(&["altinn.studio"])),
            variable("EDITOR", None),
            variable("MISSING", Some(&["example.com"])),
        ]);
        let values = [
            file(&[("GITHUB_TOKEN", "t"), ("AZURE_DEVOPS_PAT", "p"), ("EDITOR", "vim")]),
            file(&[("GITHUB_TOKEN", "t"), ("STUDIO_API_KEY", "s")]),
        ];
        let stored = Stored {
            logins: BTreeSet::new(),
            secrets: BTreeSet::from(["studio-api-key".to_owned()]),
        };

        let entries = entries(&moves, &values, &stored).expect("kept");

        assert_eq!(
            places(&entries),
            [
                "GITHUB_TOKEN, GIT_USER_NAME: the GitHub login default",
                "AZURE_DEVOPS_PAT: the secret azure-devops-pat, for dev.azure.com",
                "STUDIO_API_KEY: the secret studio-api-key, for altinn.studio, stored already",
                "EDITOR: the value vim, which the agent builder writes into the manifests",
            ]
        );
        let to_store = entries.iter().map(|entry| entry.store.is_some()).collect::<Vec<_>>();
        assert_eq!(to_store, [true, true, false, false]);
    }

    #[test]
    fn a_value_the_files_disagree_on_is_left_to_the_person() {
        let moves = agent::exposed::moves(&[variable("AZURE_DEVOPS_PAT", Some(&["dev.azure.com"]))]);
        let values = [
            file(&[("AZURE_DEVOPS_PAT", "first-secret")]),
            file(&[("AZURE_DEVOPS_PAT", "second-secret")]),
        ];
        let unkept = entries(&moves, &values, &NOTHING).err().expect("unkept");
        assert_eq!(unkept.len(), 1);
        assert!(
            unkept[0].contains("agentctl secret set azure-devops-pat --hosts dev.azure.com"),
            "says how to store it: {unkept:?}"
        );
        assert!(!unkept[0].contains("first-secret"), "never shows a secret: {unkept:?}");

        // Once the person has stored it, the files no longer need to agree.
        let stored = Stored {
            logins: BTreeSet::new(),
            secrets: BTreeSet::from(["azure-devops-pat".to_owned()]),
        };
        assert!(entries(&moves, &values, &stored).is_ok());
    }

    #[test]
    fn a_second_github_token_is_stored_as_a_secret_of_its_own() {
        let token = |source: &str| Variable {
            source: Some(source.into()),
            ..variable("GITHUB_TOKEN", Some(&["api.github.com"]))
        };
        let moves = agent::exposed::moves(&[token("DEV_GH"), token("PROD_GH")]);
        let values = [file(&[("DEV_GH", "dev"), ("PROD_GH", "prod")])];

        let entries = entries(&moves, &values, &NOTHING).expect("kept");

        assert_eq!(
            places(&entries),
            [
                "GITHUB_TOKEN: the GitHub login default",
                "GITHUB_TOKEN: the secret prod-gh, for api.github.com",
            ]
        );
        assert!(entries.iter().all(|entry| entry.store.is_some()), "both are stored");
    }

    #[test]
    fn a_value_one_manifest_reads_as_a_secret_is_never_shown_as_a_plain_one() {
        let plain = variable("API_KEY", None);
        let secret = variable("API_KEY", Some(&["example.com"]));
        let values = [file(&[("API_KEY", "key-value")])];
        for variables in [[plain.clone(), secret.clone()], [secret, plain]] {
            let unkept = entries(&agent::exposed::moves(&variables), &values, &NOTHING)
                .err()
                .expect("left to the person");
            assert!(unkept.iter().all(|line| !line.contains("key-value")), "{unkept:?}");
            assert!(unkept.iter().any(|line| line.contains("as a secret")), "{unkept:?}");
        }
    }

    #[test]
    fn where_a_secret_goes_never_shows_it() {
        let moves = agent::exposed::moves(&[
            variable("GITHUB_TOKEN", Some(&["github.com"])),
            variable("API_KEY", Some(&["example.com"])),
        ]);
        let values = [file(&[("GITHUB_TOKEN", "token-value"), ("API_KEY", "key-value")])];
        let shown = places(&entries(&moves, &values, &NOTHING).expect("kept")).join("\n");
        assert!(
            !shown.contains("token-value") && !shown.contains("key-value"),
            "{shown}"
        );
    }

    #[test]
    fn what_a_manifest_that_cannot_be_read_may_take_is_what_the_files_hold_besides() {
        let environment_file = |readers| EnvironmentFile {
            readers,
            stored: Vec::new(),
        };
        let unreadable = Reader::Unreadable {
            name: "agent.mine.yaml".into(),
            error: String::new(),
        };
        let moving = [Variable {
            source: Some("KEY".into()),
            ..variable("API_KEY", Some(&["example.com"]))
        }];
        let values = [file(&[("KEY", "k"), ("OTHER", "o")]), file(&[("KEY", "k")])];

        let line = unread(&[environment_file(vec![unreadable])], &moving, &values).expect("named");
        assert!(
            line.contains("agent.mine.yaml") && line.contains("OTHER") && !line.contains("KEY,"),
            "{line}"
        );
        assert_eq!(unread(&[environment_file(Vec::new())], &moving, &values), None);
    }
}
