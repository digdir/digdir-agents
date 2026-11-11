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
/// to change, so the person moves those off first, before anything here changes.
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
    let (entries, others) = plan(client, &read).await?;
    let shown = read.iter().map(|file| file.shown(from.as_deref())).collect::<Vec<_>>();
    let mut lines = vec![
        "agentctl update gives the agent builder this checkout, but these environment files in it hold \
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
    let deleted =
        consent::remove(&paths).map_err(|error| CommandError::Message(format!("the keys are stored, but {error}")))?;
    println!("Deleted {}.", agent::exposed::sentence_list(&deleted));
    // Also where the person sees it, in case the builder does not start.
    if !table.is_empty() {
        println!("The agent builder has the manifests take each value from here:");
        for line in &table {
            println!("{line}");
        }
    }
    let agents = EnvironmentFile::agents(read.iter().filter_map(|file| file.environment_file.as_ref()));
    if !agents.is_empty() {
        println!(
            "Apply {} again once the manifests take their values from there, so they no longer read the files.",
            agent::exposed::sentence_list(&agents)
        );
    }
    Ok(Some(told(&entries, &agents)))
}

/// Where each value the environment files `read` hold goes, or what the person stores by hand
/// first, with what else the files hold, which goes with them.
async fn plan(client: &Client, read: &[&ExposedFile]) -> CommandResult<(Vec<Entry>, Option<String>)> {
    let environment = || read.iter().filter_map(|file| file.environment_file.as_ref());
    let moving = EnvironmentFile::moving(environment());
    let mut values = Vec::new();
    for file in read {
        values.push(
            agent::environment::read(&file.path)
                .await
                .map_err(|error| CommandError::Message(format!("{}: {error}", file.path.display())))?,
        );
    }
    let kept = kept(client).await?;
    let by_hand = |unkept: Vec<String>| {
        CommandError::Message(format!(
            "these values in the environment files need storing by hand:\n  {}",
            unkept.join("\n  ")
        ))
    };
    let mut moves = agent::exposed::moves(&moving);
    moves.extend(taken(environment(), &moves, &values));
    let moves = without_token(moves, &values, &kept);
    let entries = entries(&moves, &values, &kept).map_err(by_hand)?;
    let unmet = unmet(environment(), &entries, &kept);
    if !unmet.is_empty() {
        return Err(by_hand(unmet));
    }
    Ok((entries, others(environment(), &moves, &values)))
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

/// What else the `files` hold, given their `values`: every variable none of `moves` takes, which
/// is deleted with them unstored. A manifest beside them that agentctl cannot read may take any of
/// those, but only those, so they are named whether or not one does.
fn others<'a>(files: impl IntoIterator<Item = &'a EnvironmentFile>, moves: &[Move], values: &Values) -> Option<String> {
    let manifests = files
        .into_iter()
        .flat_map(|file| &file.readers)
        .filter_map(|reader| match reader {
            Reader::Unreadable { name, .. } => Some(name.clone()),
            Reader::Manifest { .. } | Reader::Agent { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    let unreadable = agent::exposed::sentence_list(&manifests.into_iter().collect::<Vec<_>>());
    // Git's name and email go with the login, so they are taken too.
    let taken = moves
        .iter()
        .flat_map(|change| {
            std::iter::once(change.source()).chain(
                change
                    .variables()
                    .iter()
                    .map(|variable| variable.source.as_deref().unwrap_or(&variable.name)),
            )
        })
        .collect::<BTreeSet<_>>();
    let others = values
        .iter()
        .flat_map(BTreeMap::keys)
        .filter(|name| !taken.contains(name.as_str()))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if others.is_empty() {
        return (!unreadable.is_empty())
            .then(|| format!("agentctl cannot read {unreadable}, but the files hold nothing else it could take."));
    }
    let readers = if unreadable.is_empty() {
        "nothing here takes".to_owned()
    } else {
        format!("only {unreadable}, which agentctl cannot read, may take")
    };
    Some(format!(
        "The files also hold {}, which {readers}. They are deleted with the files and not stored, so copy any you \
         need first.",
        agent::exposed::sentence_list(&others)
    ))
}

/// The moves for what the manifests beside the `files` take from agentctl that none of `moves`
/// stores, from the variable in the files it is named after, as [`agent::exposed::moves`] names
/// it. A manifest moved off the file, such as one pulled since, no longer says which variable it
/// took, but one that `agentctl update` moved took the one its secret or login is named after.
fn taken<'a>(files: impl IntoIterator<Item = &'a EnvironmentFile>, moves: &[Move], values: &Values) -> Vec<Move> {
    let github_token = agent::github::ACCESS_ENVIRONMENT;
    let variables = values.iter().flat_map(BTreeMap::keys).collect::<BTreeSet<_>>();
    let needed = files.into_iter().flat_map(|file| &file.stored).collect::<Vec<_>>();
    // Storing a secret sets its hosts, so it takes those of every entry that names it.
    let hosts = |name: &str| {
        let mut hosts = Vec::<String>::new();
        for needed in &needed {
            if let agent::exposed::Stored::Secret {
                name: other,
                allowed_hosts,
                ..
            } = needed
                && other == name
            {
                for host in allowed_hosts {
                    if !hosts.contains(host) {
                        hosts.push(host.clone());
                    }
                }
            }
        }
        hosts
    };
    let mut taken = Vec::<Move>::new();
    for needed in needed.iter().copied() {
        let covered = moves.iter().chain(&taken).any(|change| match (change, needed) {
            (Move::GithubLogin { .. }, agent::exposed::Stored::GithubLogin { login }) => login == MOVED_LOGIN,
            (Move::Secret { name, .. }, agent::exposed::Stored::Secret { name: needed, .. }) => {
                name.as_ref() == Some(needed)
            }
            _ => false,
        });
        if covered {
            continue;
        }
        match needed {
            agent::exposed::Stored::GithubLogin { login }
                if login == MOVED_LOGIN && variables.iter().any(|variable| *variable == github_token) =>
            {
                taken.push(Move::GithubLogin {
                    source: github_token.into(),
                    variables: vec![Variable {
                        name: github_token.into(),
                        source: None,
                        allowed_hosts: Some(Vec::new()),
                    }],
                });
            }
            agent::exposed::Stored::Secret { name, .. } => {
                let Some(source) = variables
                    .iter()
                    .find(|variable| agent::exposed::secret_name(variable) == *name)
                else {
                    continue;
                };
                taken.push(Move::Secret {
                    variable: Variable {
                        name: (*source).clone(),
                        source: None,
                        allowed_hosts: Some(hosts(name)),
                    },
                    source: (*source).clone(),
                    name: Some(name.clone()),
                    hosts: hosts(name),
                });
            }
            agent::exposed::Stored::GithubLogin { .. } => {}
        }
    }
    taken
}

/// `moves` with Git's name and email as plain values again when there is no GitHub token to keep
/// them with: no login is stored, and the files hold no token or an empty one, as for an optional
/// one left blank.
fn without_token(moves: Vec<Move>, values: &Values, kept: &Kept) -> Vec<Move> {
    let has = |source: &str| {
        values
            .iter()
            .any(|values| values.get(source).is_some_and(|value| !value.is_empty()))
    };
    moves
        .into_iter()
        .flat_map(|change| match change {
            Move::GithubLogin {
                ref source,
                ref variables,
            } if !kept.logins.contains(MOVED_LOGIN) && !has(source) => variables
                .iter()
                .filter(|variable| variable.allowed_hosts.is_none())
                .map(|variable| Move::Plain {
                    variable: variable.clone(),
                })
                .collect(),
            change => vec![change],
        })
        .collect()
}

/// What the manifests beside the `files` take from agentctl that is neither kept already nor among
/// the `entries` to store, such as the secret a manifest pulled since names. Deleting the files
/// would lose its value, so the person stores it first. An optional secret is not missed, as one
/// the person has no key for is left out.
fn unmet<'a>(files: impl IntoIterator<Item = &'a EnvironmentFile>, entries: &[Entry], kept: &Kept) -> Vec<String> {
    let storing = |login: Option<&str>, secret: Option<&str>| {
        entries.iter().any(|entry| match &entry.store {
            Some(Store::Login(_)) => login == Some(MOVED_LOGIN),
            Some(Store::Secret { name, .. }) => secret == Some(name.as_str()),
            None => false,
        })
    };
    let mut unmet = BTreeSet::new();
    for needed in files.into_iter().flat_map(|file| &file.stored) {
        match needed {
            agent::exposed::Stored::GithubLogin { login }
                if !kept.logins.contains(login) && !storing(Some(login), None) =>
            {
                unmet.insert(format!(
                    "the GitHub login {login}: the manifests take it, but it is not stored. Sign in with `agentctl \
                     github login {login}`, then run this again"
                ));
            }
            agent::exposed::Stored::Secret {
                name,
                allowed_hosts,
                optional: false,
            } if !kept.secrets.contains(name) && !storing(None, Some(name)) => {
                unmet.insert(format!(
                    "the secret {name}: the manifests take it, but it is not stored. Store it with `{}`, then run \
                     this again",
                    agent::exposed::secret_set(name, allowed_hosts)
                ));
            }
            agent::exposed::Stored::GithubLogin { .. } | agent::exposed::Stored::Secret { .. } => {}
        }
    }
    unmet.into_iter().collect()
}

/// The names of the GitHub logins and the secrets agentctl keeps already.
struct Kept {
    logins: BTreeSet<String>,
    secrets: BTreeSet<String>,
}

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

/// The variables agents get the value of `change` as, with the file's name for it where that
/// differs, as the manifests still name it.
fn what(change: &Move) -> String {
    change
        .variables()
        .iter()
        .map(|variable| match &variable.source {
            Some(source) if *source != variable.name => format!("{} ({source} in the file)", variable.name),
            _ => variable.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Where each of `moves` goes, in their order, given the `values` of each file and what is `kept`
/// already, or what agentctl cannot store, each with what the person does instead. A secret no file
/// holds a value for, such as an optional one left blank, is named still, as others who pull the
/// manifests may have one. A plain value no file holds is left out.
fn entries(moves: &[Move], values: &Values, kept: &Kept) -> Result<Vec<Entry>, Vec<String>> {
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
        let what = what(change);
        // Where it goes, whether it is there already, and how the person puts it there.
        let (place, there, by_hand) = match change {
            Move::GithubLogin { .. } => (
                format!("the GitHub login {MOVED_LOGIN}"),
                kept.logins.contains(MOVED_LOGIN),
                format!("Sign in with `agentctl github login {MOVED_LOGIN}`"),
            ),
            // One stored already needs neither a name made here nor hosts.
            Move::Secret { name: Some(name), .. } if kept.secrets.contains(name) => {
                (format!("the secret {name}"), true, String::new())
            }
            Move::Secret { name, hosts, .. } => {
                let command = agent::exposed::secret_set(name.as_deref().unwrap_or("NAME"), hosts);
                let Some(name) = name.as_deref().filter(|_| !hosts.is_empty()) else {
                    unkept.push(format!(
                        "{source}: agentctl cannot tell what to store it as. Store it with `{command}`, then run this \
                         again"
                    ));
                    continue;
                };
                (
                    format!("the secret {name}, for {}", hosts.join(", ")),
                    false,
                    format!("Store the one agents should get with `{command}`"),
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
        // The files are one place for a value, so they must agree on it. An empty value is none, as
        // an optional secret takes it.
        let mut found = values
            .iter()
            .filter_map(|values| values.get(source))
            .filter(|value| !value.is_empty());
        let value = found.next();
        if found.any(|other| Some(other) != value) {
            unkept.push(format!(
                "{source}: the files hold different values. {by_hand}, then run this again"
            ));
            continue;
        }
        let Some(value) = value.cloned() else {
            if matches!(change, Move::Secret { .. }) {
                entries.push(Entry {
                    what,
                    place: format!("{place}, with no value in the files"),
                    store: None,
                });
            }
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
            // Only a plain value is ever shown.
            Move::Plain { .. } => (
                format!(
                    "the value {}, which the agent builder writes into the manifests",
                    value.as_str()
                ),
                None,
            ),
            Move::Secret { name: None, .. } => continue,
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

    const NOTHING: Kept = Kept {
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
        let stored = Kept {
            logins: BTreeSet::new(),
            secrets: BTreeSet::from(["studio-api-key".to_owned()]),
        };

        let entries = entries(&moves, &values, &stored).expect("kept");

        assert_eq!(
            places(&entries),
            [
                "GITHUB_TOKEN, GIT_USER_NAME: the GitHub login default",
                "AZURE_DEVOPS_PAT: the secret azure-devops-pat, for dev.azure.com",
                "STUDIO_API_KEY: the secret studio-api-key, stored already",
                "EDITOR: the value vim, which the agent builder writes into the manifests",
                // Named still, for others who pull the manifests and have one.
                "MISSING: the secret missing, for example.com, with no value in the files",
            ]
        );
        let to_store = entries.iter().map(|entry| entry.store.is_some()).collect::<Vec<_>>();
        assert_eq!(to_store, [true, true, false, false, false]);
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
        let stored = Kept {
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
                "GITHUB_TOKEN (DEV_GH in the file): the GitHub login default",
                "GITHUB_TOKEN (PROD_GH in the file): the secret prod-gh, for api.github.com",
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
    fn an_empty_value_is_none_and_a_renamed_one_names_its_variable_in_the_file() {
        let renamed = |source: &str| Variable {
            source: Some(source.into()),
            ..variable("GIT_AUTHOR_NAME", None)
        };
        let moves = agent::exposed::moves(&[
            variable("AIVAR_API_KEY", Some(&["api.aivar.no"])),
            renamed("WORK_NAME"),
            renamed("PERSONAL_NAME"),
        ]);
        let values = [
            file(&[("AIVAR_API_KEY", ""), ("WORK_NAME", "Alice")]),
            file(&[("AIVAR_API_KEY", "key"), ("PERSONAL_NAME", "A.")]),
        ];

        let entries = entries(&moves, &values, &NOTHING).expect("kept");

        assert_eq!(
            places(&entries),
            [
                "AIVAR_API_KEY: the secret aivar-api-key, for api.aivar.no",
                "GIT_AUTHOR_NAME (WORK_NAME in the file): the value Alice, which the agent builder writes into the \
                 manifests",
                "GIT_AUTHOR_NAME (PERSONAL_NAME in the file): the value A., which the agent builder writes into the \
                 manifests",
            ],
            "an empty value does not disagree with another"
        );
        let blank = super::entries(&moves[..1], &[file(&[("AIVAR_API_KEY", "")])], &NOTHING).expect("nothing to keep");
        assert_eq!(
            places(&blank),
            ["AIVAR_API_KEY: the secret aivar-api-key, for api.aivar.no, with no value in the files"]
        );
        assert!(
            blank.iter().all(|entry| entry.store.is_none()),
            "an empty value is never stored"
        );
    }

    #[test]
    fn what_a_pulled_manifest_takes_is_stored_before_its_file_goes() {
        // An agent applied before its manifest was moved off the file, which a pull brought.
        let file = |secret: &str| EnvironmentFile {
            readers: vec![Reader::Agent {
                name: "agent/dev".into(),
                variables: vec![variable("AZURE_DEVOPS_PAT", Some(&["dev.azure.com"]))],
            }],
            stored: vec![agent::exposed::Stored::Secret {
                name: secret.into(),
                allowed_hosts: vec!["dev.azure.com".into()],
                optional: false,
            }],
        };
        let values = [self::file(&[("AZURE_DEVOPS_PAT", "pat")])];
        let planned = |file: &EnvironmentFile| {
            let moving = EnvironmentFile::moving([file]);
            let entries = entries(&agent::exposed::moves(&moving), &values, &NOTHING).expect("kept");
            (places(&entries), unmet([file], &entries, &NOTHING))
        };

        let (places, unmet) = planned(&file("azure-devops-pat"));
        assert_eq!(
            places,
            ["AZURE_DEVOPS_PAT: the secret azure-devops-pat, for dev.azure.com"]
        );
        assert!(unmet.is_empty(), "{unmet:?}");

        let (_, unmet) = planned(&file("ado-pat"));
        assert_eq!(unmet.len(), 1, "a name the files cannot tell: {unmet:?}");
        assert!(
            unmet[0].contains("agentctl secret set ado-pat --hosts dev.azure.com"),
            "{unmet:?}"
        );
    }

    #[test]
    fn what_migrated_manifests_take_comes_from_the_variable_it_is_named_after() {
        // A coworker's file once their pull moved the manifests beside it off the file, with no
        // agent applied with it.
        let pulled = EnvironmentFile {
            readers: Vec::new(),
            stored: vec![
                agent::exposed::Stored::GithubLogin {
                    login: MOVED_LOGIN.into(),
                },
                agent::exposed::Stored::Secret {
                    name: "azure-devops-pat".into(),
                    allowed_hosts: vec!["dev.azure.com".into()],
                    optional: false,
                },
                agent::exposed::Stored::Secret {
                    name: "ado-other".into(),
                    allowed_hosts: vec!["dev.azure.com".into()],
                    optional: false,
                },
            ],
        };
        let values = [file(&[
            ("GITHUB_TOKEN", "token"),
            ("AZURE_DEVOPS_PAT", "pat"),
            ("UNUSED", "x"),
        ])];

        let moves = taken([&pulled], &[], &values);
        let entries = entries(&moves, &values, &NOTHING).expect("kept");

        assert_eq!(
            places(&entries),
            [
                "GITHUB_TOKEN: the GitHub login default",
                "AZURE_DEVOPS_PAT: the secret azure-devops-pat, for dev.azure.com",
            ]
        );
        let unmet = unmet([&pulled], &entries, &NOTHING);
        assert_eq!(unmet.len(), 1, "a name no variable has: {unmet:?}");
        assert!(unmet[0].contains("agentctl secret set ado-other"), "{unmet:?}");
    }

    #[test]
    fn a_pulled_secret_the_person_has_no_key_for_stops_update_only_when_it_is_required() {
        let secret = |name: &str, hosts: &[&str], optional: bool| agent::exposed::Stored::Secret {
            name: name.into(),
            allowed_hosts: hosts.iter().map(|&host| host.to_owned()).collect(),
            optional,
        };
        let pulled = |stored| EnvironmentFile {
            readers: Vec::new(),
            stored,
        };
        let plan = |file: &EnvironmentFile, values: &[BTreeMap<String, Zeroizing<String>>], kept: &Kept| {
            let moves = taken([file], &[], values);
            entries(&moves, values, kept).map(|entries| unmet([file], &entries, kept))
        };

        // An optional key left blank, or not in the file at all.
        let optional = pulled(vec![secret("aivar-api-key", &["api.aivar.no"], true)]);
        for values in [vec![file(&[("AIVAR_API_KEY", "")])], vec![file(&[])]] {
            assert_eq!(plan(&optional, &values, &NOTHING), Ok(Vec::new()));
        }
        let required = pulled(vec![secret("aivar-api-key", &["api.aivar.no"], false)]);
        assert_eq!(plan(&required, &[file(&[])], &NOTHING).map(|unmet| unmet.len()), Ok(1));

        // A migrated entry that leaves the hosts to the stored secret: by hand, then done.
        let without_hosts = pulled(vec![secret("aivar-api-key", &[], false)]);
        let values = [file(&[("AIVAR_API_KEY", "key")])];
        assert!(
            plan(&without_hosts, &values, &NOTHING).is_err(),
            "it cannot tell the hosts"
        );
        let kept = Kept {
            logins: BTreeSet::new(),
            secrets: BTreeSet::from(["aivar-api-key".to_owned()]),
        };
        assert_eq!(plan(&without_hosts, &values, &kept), Ok(Vec::new()), "stored by hand");
    }

    #[test]
    fn without_a_token_gits_name_and_email_stay_values_for_the_builder() {
        let moves = agent::exposed::moves(&[
            variable("GITHUB_TOKEN", Some(&["api.github.com"])),
            variable("GIT_USER_NAME", None),
        ]);
        let values = [file(&[("GITHUB_TOKEN", ""), ("GIT_USER_NAME", "Alice")])];

        let entries = entries(&without_token(moves, &values, &NOTHING), &values, &NOTHING).expect("kept");

        assert_eq!(
            places(&entries),
            ["GIT_USER_NAME: the value Alice, which the agent builder writes into the manifests"]
        );
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
    fn what_else_the_files_hold_is_named_before_it_goes_with_them() {
        let environment_file = |readers| EnvironmentFile {
            readers,
            stored: Vec::new(),
        };
        let unreadable = || Reader::Unreadable {
            name: "agent.mine.yaml".into(),
        };
        // Git's name and email go with the login, so the files hold nothing else of theirs.
        let moves = agent::exposed::moves(&[
            variable("GITHUB_TOKEN", Some(&["api.github.com"])),
            variable("GIT_USER_NAME", None),
            Variable {
                source: Some("KEY".into()),
                ..variable("API_KEY", Some(&["example.com"]))
            },
        ]);
        let values = [
            file(&[
                ("GITHUB_TOKEN", "t"),
                ("GIT_USER_NAME", "Alice"),
                ("KEY", "k"),
                ("OLD", "o"),
            ]),
            file(&[("KEY", "k")]),
        ];

        let line = others(&[environment_file(vec![unreadable()])], &moves, &values).expect("named");
        assert!(
            line.contains("The files also hold OLD, which only agent.mine.yaml")
                && line.contains("deleted with the files and not stored"),
            "{line}"
        );
        let line = others(&[environment_file(Vec::new())], &moves, &values).expect("named");
        assert!(line.contains("OLD, which nothing here takes"), "{line}");

        let held = [file(&[("GITHUB_TOKEN", "t"), ("GIT_USER_NAME", "Alice"), ("KEY", "k")])];
        assert_eq!(others(&[environment_file(Vec::new())], &moves, &held), None);
        assert!(
            others(&[environment_file(vec![unreadable()])], &moves, &held)
                .is_some_and(|line| line.contains("nothing else it could take"))
        );
    }
}
