//! Where each value an environment file holds goes when `agentctl update` moves agents off it:
//! which login or secret stores it under which name, which plain value the builder writes into the
//! manifests, and what the person stores by hand. Nothing here reads or changes anything, so every
//! rule is tested through [`plan`].

use std::collections::{BTreeMap, BTreeSet};

use agent::exposed::{EnvironmentFile, Reader, Stored, Variable, secret_set, sentence_list};
use zeroize::Zeroizing;

/// The values of each environment file, by variable.
pub(super) type Values = [BTreeMap<String, Zeroizing<String>>];

/// The names of the GitHub logins and the secrets agentctl keeps already.
pub(super) struct Kept {
    pub(super) logins: BTreeSet<String>,
    pub(super) secrets: BTreeSet<String>,
}

/// Where each value the environment files hold goes, with what else they hold, which goes with
/// them, and the agents that read them.
pub(super) struct Plan {
    pub(super) entries: Vec<Entry>,
    /// What else the files hold, said before it is deleted with them.
    pub(super) others: Option<String>,
    /// The agents applied with the files, as `agent/NAME`, which the person applies again.
    pub(super) agents: Vec<String>,
}

/// Where a value of the environment files goes, and what storing it there takes.
pub(super) struct Entry {
    /// The variables agents get it as.
    pub(super) what: String,
    /// Where the manifests take it from once the files are gone.
    pub(super) place: String,
    /// What agentctl stores, unless it is stored already or a plain value, which the builder writes
    /// into the manifests.
    pub(super) store: Option<Store>,
}

pub(super) enum Store {
    /// The GitHub token, kept as the login [`MOVED_LOGIN`].
    Login(Zeroizing<String>),
    Secret {
        name: String,
        hosts: Vec<String>,
        value: Zeroizing<String>,
    },
}

/// Where each value the environment `files` hold goes, given their `values` and what is `kept`
/// already, or what the person stores by hand first, each with the command that does.
///
/// The values come from what reads the files: their manifests, or agents applied with them. A
/// manifest that a pull has moved off the file says only what it takes from agentctl, which comes
/// from the variable it is named after. Nothing is planned that would delete a value a manifest
/// still needs.
pub(super) fn plan(files: &[&EnvironmentFile], values: &Values, kept: &Kept) -> Result<Plan, Vec<String>> {
    let files = || files.iter().copied();
    let moving = moving(files());
    let mut moves = moves(&moving);
    moves.extend(taken(files(), &moves, values));
    // The login is stored first, so the person may still decline it once GitHub says whom it acts
    // as, before anything is stored.
    moves.sort_by_key(|change| !matches!(change, Move::GithubLogin { .. }));
    let moves = without_token(moves, values, kept);
    let entries = entries(&moves, values, kept)?;
    let unmet = unmet(files(), &entries, kept);
    if !unmet.is_empty() {
        return Err(unmet);
    }
    Ok(Plan {
        others: others(files(), &moves, values),
        agents: agents(files()),
        entries,
    })
}

/// The variables of Git's name and email, which a GitHub login's account gives agents instead.
const GIT_IDENTITY: [&str; 2] = ["GIT_USER_NAME", "GIT_USER_EMAIL"];

/// The GitHub login that a GitHub token in an environment file is kept as.
pub(super) const MOVED_LOGIN: &str = "default";

/// Where a value an agent reads from an environment file goes instead.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Move {
    /// A GitHub token, kept as the GitHub login [`MOVED_LOGIN`], whose account is also Git's name
    /// and email, so the `variables` for those go with it.
    GithubLogin {
        /// The file's variable that holds the token.
        source: String,
        variables: Vec<Variable>,
    },
    /// A secret stored under `name`, for the `hosts` of every entry that reads the file's variable
    /// `source`. It has no `name` when none can be made from the variable's.
    Secret {
        variable: Variable,
        source: String,
        name: Option<String>,
        hosts: Vec<String>,
    },
    /// A plain value, which goes in the manifest.
    Plain { variable: Variable },
}

impl Move {
    /// The file's variable that holds the value.
    fn source(&self) -> &str {
        match self {
            Self::GithubLogin { source, .. } | Self::Secret { source, .. } => source,
            Self::Plain { variable } => variable.source.as_deref().unwrap_or(&variable.name),
        }
    }

    /// The variables agents get the value as.
    fn variables(&self) -> &[Variable] {
        match self {
            Self::GithubLogin { variables, .. } => variables,
            Self::Secret { variable, .. } | Self::Plain { variable } => std::slice::from_ref(variable),
        }
    }
}

/// The name a secret from the environment file's variable `source` is stored under, before what
/// keeps two of them apart, such as `api-key` for `API_KEY`.
fn secret_name(source: &str) -> String {
    source.to_ascii_lowercase().replace('_', "-")
}

/// Where each of `variables` goes instead of the environment file.
///
/// A secret is named after the file's variable, which is what tells one credential from another,
/// whichever variables agents get it as.
fn moves(variables: &[Variable]) -> Vec<Move> {
    let credential = |variable: &Variable| variable.source.clone().unwrap_or_else(|| variable.name.clone());
    let is_token =
        |variable: &Variable| variable.allowed_hosts.is_some() && variable.name == agent::github::ACCESS_ENVIRONMENT;
    // One token becomes the login. A token from another of the file's variables is another
    // credential, so it is stored as a secret of its own rather than lost with the file.
    let login = variables.iter().find(|variable| is_token(variable)).map(credential);
    let (github, others): (Vec<_>, Vec<_>) = variables.iter().partition(|variable| {
        login.as_ref().is_some_and(|login| {
            (is_token(variable) && credential(variable) == *login)
                || (variable.allowed_hosts.is_none() && GIT_IDENTITY.contains(&variable.name.as_str()))
        })
    });
    let mut moves = Vec::new();
    if let Some(source) = login {
        moves.push(Move::GithubLogin {
            source,
            variables: github.iter().copied().cloned().collect(),
        });
    }
    // Storing a secret sets its hosts, so it takes those of every entry that reads it.
    let hosts = |source: &str| {
        let mut hosts = Vec::<String>::new();
        for variable in others.iter().filter(|variable| credential(variable) == source) {
            for host in variable.allowed_hosts.iter().flatten() {
                if !hosts.contains(host) {
                    hosts.push(host.clone());
                }
            }
        }
        hosts
    };
    let mut named = Vec::<(String, String)>::new();
    let mut seen = Vec::<(&str, &Option<String>, bool)>::new();
    for variable in &others {
        // Entries that differ only in their hosts are one entry to change, but a secret and a plain
        // value are two, even of the same variable.
        let entry = (
            variable.name.as_str(),
            &variable.source,
            variable.allowed_hosts.is_some(),
        );
        if seen.contains(&entry) {
            continue;
        }
        seen.push(entry);
        if variable.allowed_hosts.is_none() {
            moves.push(Move::Plain {
                variable: (*variable).clone(),
            });
            continue;
        }
        let source = credential(variable);
        let name = secret_name(&source);
        // Two of the file's variables may differ only in what a name leaves out, such as case.
        let taken = |name: &str| named.iter().any(|(other, taken)| taken == name && *other != source);
        let name = std::iter::once(name.clone())
            .chain((2..named.len() + 2).map(|number| format!("{name}-{number}")))
            .find(|name| !taken(name))
            .filter(|name| agent::names::valid(name));
        if let Some(name) = &name {
            named.push((source.clone(), name.clone()));
        }
        moves.push(Move::Secret {
            variable: (*variable).clone(),
            hosts: hosts(&source),
            source,
            name,
        });
    }
    moves
}

/// The values to move out of the `files` before they can go: those their readers read, each
/// once. That includes what an agent reads whose manifest no longer does, such as one pulled
/// since, as the values are in the file alone until the agent is applied again.
fn moving<'a>(files: impl IntoIterator<Item = &'a EnvironmentFile>) -> Vec<Variable> {
    let mut moving = Vec::<Variable>::new();
    for reader in files.into_iter().flat_map(|file| &file.readers) {
        let variables = match reader {
            Reader::Manifest { variables, .. } | Reader::Agent { variables, .. } => variables.as_slice(),
            Reader::Unreadable { .. } => &[],
        };
        for variable in variables {
            if !moving.contains(variable) {
                moving.push(variable.clone());
            }
        }
    }
    moving
}

/// The agents that read the `files`, each once as `agent/NAME`, which are applied again once
/// they no longer read them.
fn agents<'a>(files: impl IntoIterator<Item = &'a EnvironmentFile>) -> Vec<String> {
    let mut agents = Vec::<String>::new();
    for reader in files.into_iter().flat_map(|file| &file.readers) {
        if let Reader::Agent { name, .. } = reader
            && !agents.contains(name)
        {
            agents.push(name.clone());
        }
    }
    agents
}

/// What else the `files` hold, given their `values`: every variable none of `moves` takes, which
/// is deleted with them unstored. A manifest beside them that agentctl cannot read may take any of
/// those, but only those, so they are named whether or not one does.
fn others<'a>(files: impl IntoIterator<Item = &'a EnvironmentFile>, moves: &[Move], values: &Values) -> Option<String> {
    let files = files.into_iter().collect::<Vec<_>>();
    let manifests = files
        .iter()
        .flat_map(|file| &file.readers)
        .filter_map(|reader| match reader {
            Reader::Unreadable { name, .. } => Some(name.clone()),
            Reader::Manifest { .. } | Reader::Agent { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    let unreadable = sentence_list(&manifests.into_iter().collect::<Vec<_>>());
    let mut taken = moves
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
    // A GitHub login's account gives agents Git's name and email, so those are not lost with it.
    let login = moves.iter().any(|change| matches!(change, Move::GithubLogin { .. }))
        || files
            .iter()
            .flat_map(|file| &file.stored)
            .any(|stored| matches!(stored, Stored::GithubLogin { .. }));
    if login {
        taken.extend(GIT_IDENTITY);
    }
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
        sentence_list(&others)
    ))
}

/// The moves for what the manifests beside the `files` take from agentctl that none of `moves`
/// stores, such as what a manifest that a pull has moved off the file takes. A secret's value is in
/// the variable its entry gives agents, or, for an entry that renamed one with `source`, in the
/// variable its name was made from, as [`moves`] names it. The login's is in `GITHUB_TOKEN`.
fn taken<'a>(files: impl IntoIterator<Item = &'a EnvironmentFile>, moves: &[Move], values: &Values) -> Vec<Move> {
    let github_token = agent::github::ACCESS_ENVIRONMENT;
    let variables = values.iter().flat_map(BTreeMap::keys).collect::<BTreeSet<_>>();
    let needed = files.into_iter().flat_map(|file| &file.stored).collect::<Vec<_>>();
    // Storing a secret sets its hosts, so it takes those of every entry that names it.
    let hosts = |name: &str| {
        let mut hosts = Vec::<String>::new();
        for needed in &needed {
            if let Stored::Secret {
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
            (Move::GithubLogin { .. }, Stored::GithubLogin { login }) => login == MOVED_LOGIN,
            (Move::Secret { name, .. }, Stored::Secret { name: needed, .. }) => name.as_ref() == Some(needed),
            _ => false,
        });
        if covered {
            continue;
        }
        match needed {
            Stored::GithubLogin { login }
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
            Stored::Secret { name, environment, .. } => {
                let Some(source) = variables
                    .iter()
                    .find(|variable| **variable == environment)
                    .or_else(|| variables.iter().find(|variable| secret_name(variable) == *name))
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
            Stored::GithubLogin { .. } => {}
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
            Stored::GithubLogin { login } if !kept.logins.contains(login) && !storing(Some(login), None) => {
                unmet.insert(format!(
                    "the GitHub login {login}: the manifests take it, but it is not stored. Sign in with `agentctl \
                     github login {login}`, then run this again"
                ));
            }
            Stored::Secret {
                name,
                allowed_hosts,
                optional: false,
                ..
            } if !kept.secrets.contains(name) && !storing(None, Some(name)) => {
                unmet.insert(format!(
                    "the secret {name}: the manifests take it, but it is not stored. Store it with `{}`, then run \
                     this again",
                    secret_set(name, allowed_hosts)
                ));
            }
            Stored::GithubLogin { .. } | Stored::Secret { .. } => {}
        }
    }
    unmet.into_iter().collect()
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

/// The value the files hold for the variable `source`, or `Err` when they hold different ones: the
/// files are one place for a value, so they must agree on it. An empty value is none, as an
/// optional secret takes it.
fn value<'a>(values: &'a Values, source: &str) -> Result<Option<&'a Zeroizing<String>>, ()> {
    let mut found = values
        .iter()
        .filter_map(|values| values.get(source))
        .filter(|value| !value.is_empty());
    let value = found.next();
    if found.any(|other| Some(other) != value) {
        return Err(());
    }
    Ok(value)
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
            Move::Secret {
                name: Some(name),
                hosts,
                ..
            } if !hosts.is_empty() => (
                format!("the secret {name}, for {}", hosts.join(", ")),
                false,
                format!("Store the one agents should get with `{}`", secret_set(name, hosts)),
            ),
            // Without a value there is nothing to store, so nothing that storing needs.
            Move::Secret { name: Some(name), .. } if value(values, source) == Ok(None) => {
                (format!("the secret {name}"), false, String::new())
            }
            Move::Secret { name, hosts, .. } => {
                unkept.push(format!(
                    "{source}: agentctl cannot tell what to store it as. Store it with `{}`, then run this again",
                    secret_set(name.as_deref().unwrap_or("NAME"), hosts)
                ));
                continue;
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
        let Ok(value) = value(values, source) else {
            unkept.push(format!(
                "{source}: the files hold different values. {by_hand}, then run this again"
            ));
            continue;
        };
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

    const NOTHING: Kept = Kept {
        logins: BTreeSet::new(),
        secrets: BTreeSet::new(),
    };

    fn variable(name: &str, hosts: Option<&[&str]>) -> Variable {
        Variable {
            name: name.into(),
            source: None,
            allowed_hosts: hosts.map(|hosts| hosts.iter().map(|&host| host.to_owned()).collect()),
        }
    }

    /// `variable` as a manifest names it in the file with `source:`.
    fn renamed(name: &str, source: &str, hosts: Option<&[&str]>) -> Variable {
        Variable {
            source: Some(source.into()),
            ..variable(name, hosts)
        }
    }

    fn values(values: &[(&str, &str)]) -> BTreeMap<String, Zeroizing<String>> {
        values
            .iter()
            .map(|&(name, value)| (name.to_owned(), Zeroizing::new(value.to_owned())))
            .collect()
    }

    /// A file the manifests beside it read `variables` from.
    fn read(variables: &[Variable]) -> EnvironmentFile {
        EnvironmentFile {
            readers: vec![Reader::Manifest {
                name: "agent.yaml".into(),
                variables: variables.to_vec(),
            }],
            stored: Vec::new(),
        }
    }

    /// A file a pull moved the manifests beside off, which now take `stored` from agentctl.
    fn pulled(stored: Vec<Stored>) -> EnvironmentFile {
        EnvironmentFile {
            readers: Vec::new(),
            stored,
        }
    }

    fn secret(name: &str, hosts: &[&str], optional: bool) -> Stored {
        Stored::Secret {
            name: name.into(),
            environment: String::new(),
            allowed_hosts: hosts.iter().map(|&host| host.to_owned()).collect(),
            optional,
        }
    }

    fn kept(secrets: &[&str]) -> Kept {
        Kept {
            logins: BTreeSet::new(),
            secrets: secrets.iter().map(|&name| name.to_owned()).collect(),
        }
    }

    fn places(plan: &Plan) -> Vec<String> {
        plan.entries
            .iter()
            .map(|entry| format!("{}: {}", entry.what, entry.place))
            .collect()
    }

    #[test]
    fn each_value_goes_where_its_kind_belongs() {
        let file = read(&[
            variable("GITHUB_TOKEN", Some(&["api.github.com"])),
            variable("GIT_USER_NAME", None),
            variable("AZURE_DEVOPS_PAT", Some(&["dev.azure.com"])),
            variable("STUDIO_API_KEY", Some(&["altinn.studio"])),
            variable("EDITOR", None),
            variable("MISSING", Some(&["example.com"])),
        ]);
        let held = [
            values(&[("GITHUB_TOKEN", "t"), ("AZURE_DEVOPS_PAT", "p"), ("EDITOR", "vim")]),
            values(&[("GITHUB_TOKEN", "t"), ("STUDIO_API_KEY", "s")]),
        ];

        let plan = plan(&[&file], &held, &kept(&["studio-api-key"])).expect("planned");

        assert_eq!(
            places(&plan),
            [
                "GITHUB_TOKEN, GIT_USER_NAME: the GitHub login default",
                "AZURE_DEVOPS_PAT: the secret azure-devops-pat, for dev.azure.com",
                "STUDIO_API_KEY: the secret studio-api-key, stored already",
                "EDITOR: the value vim, which the agent builder writes into the manifests",
                // Named still, for others who pull the manifests and have one.
                "MISSING: the secret missing, for example.com, with no value in the files",
            ]
        );
        let to_store = plan
            .entries
            .iter()
            .map(|entry| entry.store.is_some())
            .collect::<Vec<_>>();
        assert_eq!(to_store, [true, true, false, false, false]);
    }

    #[test]
    fn a_credential_is_stored_once_under_a_name_of_its_own() {
        let file = read(&[
            // The same variable from several of the file's is as many credentials, and one that
            // differs only in case still gets a name of its own.
            renamed("API_KEY", "DEV_KEY", Some(&["a.example.com"])),
            renamed("API_KEY", "Dev_KEY", Some(&["a.example.com"])),
            // One credential at several hosts is stored once, for all of them.
            variable("SHARED", Some(&["a.example.com"])),
            renamed("OTHER", "SHARED", Some(&["b.example.com"])),
        ]);
        let held = [values(&[("DEV_KEY", "1"), ("Dev_KEY", "2"), ("SHARED", "3")])];

        let plan = plan(&[&file], &held, &NOTHING).expect("planned");

        assert_eq!(
            places(&plan),
            [
                "API_KEY (DEV_KEY in the file): the secret dev-key, for a.example.com",
                "API_KEY (Dev_KEY in the file): the secret dev-key-2, for a.example.com",
                "SHARED: the secret shared, for a.example.com, b.example.com",
                "OTHER (SHARED in the file): the secret shared, for a.example.com, b.example.com",
            ]
        );
    }

    #[test]
    fn a_value_the_files_disagree_on_is_left_to_the_person_until_it_is_stored() {
        let file = read(&[variable("AZURE_DEVOPS_PAT", Some(&["dev.azure.com"]))]);
        let held = [
            values(&[("AZURE_DEVOPS_PAT", "first-secret")]),
            values(&[("AZURE_DEVOPS_PAT", "second-secret")]),
        ];

        let unkept = plan(&[&file], &held, &NOTHING).err().expect("left to the person");
        assert_eq!(unkept.len(), 1);
        assert!(
            unkept[0].contains("agentctl secret set azure-devops-pat --hosts dev.azure.com"),
            "says how to store it: {unkept:?}"
        );
        assert!(!unkept[0].contains("first-secret"), "never shows a secret: {unkept:?}");

        assert!(plan(&[&file], &held, &kept(&["azure-devops-pat"])).is_ok());
    }

    #[test]
    fn a_second_github_token_is_stored_as_a_secret_of_its_own() {
        let file = read(&[
            renamed("GITHUB_TOKEN", "DEV_GH", Some(&["api.github.com"])),
            renamed("GITHUB_TOKEN", "PROD_GH", Some(&["api.github.com"])),
        ]);
        let held = [values(&[("DEV_GH", "dev"), ("PROD_GH", "prod")])];

        let plan = plan(&[&file], &held, &NOTHING).expect("planned");

        assert_eq!(
            places(&plan),
            [
                "GITHUB_TOKEN (DEV_GH in the file): the GitHub login default",
                "GITHUB_TOKEN (PROD_GH in the file): the secret prod-gh, for api.github.com",
            ]
        );
        assert!(
            plan.entries.iter().all(|entry| entry.store.is_some()),
            "both are stored"
        );
    }

    #[test]
    fn a_value_one_manifest_reads_as_a_secret_is_never_shown_as_a_plain_one() {
        let plain = variable("API_KEY", None);
        let secret = variable("API_KEY", Some(&["example.com"]));
        let held = [values(&[("API_KEY", "key-value")])];
        for variables in [[plain.clone(), secret.clone()], [secret, plain]] {
            let unkept = plan(&[&read(&variables)], &held, &NOTHING)
                .err()
                .expect("left to the person");
            assert!(unkept.iter().all(|line| !line.contains("key-value")), "{unkept:?}");
            assert!(unkept.iter().any(|line| line.contains("as a secret")), "{unkept:?}");
        }
    }

    #[test]
    fn where_a_secret_goes_never_shows_it() {
        let file = read(&[
            variable("GITHUB_TOKEN", Some(&["github.com"])),
            variable("API_KEY", Some(&["example.com"])),
        ]);
        let held = [values(&[("GITHUB_TOKEN", "token-value"), ("API_KEY", "key-value")])];
        let shown = places(&plan(&[&file], &held, &NOTHING).expect("planned")).join("\n");
        assert!(
            !shown.contains("token-value") && !shown.contains("key-value"),
            "{shown}"
        );
    }

    #[test]
    fn an_empty_value_is_none_and_a_renamed_one_names_its_variable_in_the_file() {
        let file = read(&[
            variable("AIVAR_API_KEY", Some(&["api.aivar.no"])),
            renamed("GIT_AUTHOR_NAME", "WORK_NAME", None),
            renamed("GIT_AUTHOR_NAME", "PERSONAL_NAME", None),
        ]);
        let held = [
            values(&[("AIVAR_API_KEY", ""), ("WORK_NAME", "Alice")]),
            values(&[("AIVAR_API_KEY", "key"), ("PERSONAL_NAME", "A.")]),
        ];

        assert_eq!(
            places(&plan(&[&file], &held, &NOTHING).expect("planned")),
            [
                "AIVAR_API_KEY: the secret aivar-api-key, for api.aivar.no",
                "GIT_AUTHOR_NAME (WORK_NAME in the file): the value Alice, which the agent builder writes into the \
                 manifests",
                "GIT_AUTHOR_NAME (PERSONAL_NAME in the file): the value A., which the agent builder writes into the \
                 manifests",
            ],
            "an empty value does not disagree with another"
        );

        let blank = read(&[variable("AIVAR_API_KEY", Some(&["api.aivar.no"]))]);
        let plan = plan(&[&blank], &[values(&[("AIVAR_API_KEY", "")])], &NOTHING).expect("planned");
        assert_eq!(
            places(&plan),
            ["AIVAR_API_KEY: the secret aivar-api-key, for api.aivar.no, with no value in the files"]
        );
        assert!(
            plan.entries.iter().all(|entry| entry.store.is_none()),
            "an empty value is never stored"
        );
    }

    #[test]
    fn without_a_token_gits_name_and_email_stay_values_for_the_builder() {
        let file = read(&[
            variable("GITHUB_TOKEN", Some(&["api.github.com"])),
            variable("GIT_USER_NAME", None),
        ]);
        let held = [values(&[("GITHUB_TOKEN", ""), ("GIT_USER_NAME", "Alice")])];

        assert_eq!(
            places(&plan(&[&file], &held, &NOTHING).expect("planned")),
            ["GIT_USER_NAME: the value Alice, which the agent builder writes into the manifests"]
        );
    }

    #[test]
    fn an_agent_applied_before_a_pull_has_its_values_stored_as_the_pulled_manifests_name_them() {
        let file = |name: &str| EnvironmentFile {
            readers: vec![Reader::Agent {
                name: "agent/dev".into(),
                variables: vec![variable("AZURE_DEVOPS_PAT", Some(&["dev.azure.com"]))],
            }],
            stored: vec![secret(name, &["dev.azure.com"], false)],
        };
        let held = [values(&[("AZURE_DEVOPS_PAT", "pat")])];

        let planned = plan(&[&file("azure-devops-pat")], &held, &NOTHING).expect("planned");
        assert_eq!(
            places(&planned),
            ["AZURE_DEVOPS_PAT: the secret azure-devops-pat, for dev.azure.com"]
        );
        assert_eq!(planned.agents, ["agent/dev"], "applied again once moved");

        let unmet = plan(&[&file("ado-pat")], &held, &NOTHING)
            .err()
            .expect("a name the files cannot tell");
        assert_eq!(unmet.len(), 1, "{unmet:?}");
        assert!(
            unmet[0].contains("agentctl secret set ado-pat --hosts dev.azure.com"),
            "{unmet:?}"
        );
    }

    #[test]
    fn what_pulled_manifests_take_comes_from_the_variable_it_is_named_after() {
        let file = pulled(vec![
            Stored::GithubLogin {
                login: MOVED_LOGIN.into(),
            },
            secret("azure-devops-pat", &["dev.azure.com"], false),
            secret("ado-other", &["dev.azure.com"], false),
        ]);
        let held = [values(&[("GITHUB_TOKEN", "token"), ("AZURE_DEVOPS_PAT", "pat")])];

        let unmet = plan(&[&file], &held, &NOTHING)
            .err()
            .expect("one is named after nothing");
        assert_eq!(unmet.len(), 1, "{unmet:?}");
        assert!(unmet[0].contains("agentctl secret set ado-other"), "{unmet:?}");

        let file = pulled(vec![
            Stored::GithubLogin {
                login: MOVED_LOGIN.into(),
            },
            secret("azure-devops-pat", &["dev.azure.com"], false),
        ]);
        assert_eq!(
            places(&plan(&[&file], &held, &NOTHING).expect("planned")),
            [
                "GITHUB_TOKEN: the GitHub login default",
                "AZURE_DEVOPS_PAT: the secret azure-devops-pat, for dev.azure.com",
            ]
        );
    }

    #[test]
    fn a_pulled_secret_comes_from_the_variable_its_entry_gives_agents_whatever_its_name() {
        // As this repository's own agents take Pi's key.
        let named = |optional| Stored::Secret {
            name: "aivar".into(),
            environment: "AIVAR_API_KEY".into(),
            allowed_hosts: vec!["api.aivar.no".into()],
            optional,
        };
        let held = [values(&[("AIVAR_API_KEY", "key")])];
        for optional in [false, true] {
            let plan = plan(&[&pulled(vec![named(optional)])], &held, &NOTHING).expect("planned");
            assert_eq!(places(&plan), ["AIVAR_API_KEY: the secret aivar, for api.aivar.no"]);
            assert_eq!(plan.others, None, "nothing is lost");
        }
    }

    #[test]
    fn a_coworkers_git_name_and_email_go_with_the_login_the_pulled_manifests_take() {
        let file = pulled(vec![Stored::GithubLogin {
            login: MOVED_LOGIN.into(),
        }]);
        let held = [values(&[
            ("GIT_USER_NAME", "Alice"),
            ("GIT_USER_EMAIL", "alice@example.com"),
            ("GITHUB_TOKEN", "token"),
        ])];
        let plan = plan(&[&file], &held, &NOTHING).expect("planned");
        assert_eq!(places(&plan), ["GITHUB_TOKEN: the GitHub login default"]);
        assert_eq!(plan.others, None, "{:?}", plan.others);
    }

    #[test]
    fn the_login_is_stored_before_any_secret() {
        // An agent applied before the pull reads a key, and the pulled manifests take the login.
        let file = EnvironmentFile {
            readers: vec![Reader::Agent {
                name: "agent/dev".into(),
                variables: vec![variable("API_KEY", Some(&["example.com"]))],
            }],
            stored: vec![Stored::GithubLogin {
                login: MOVED_LOGIN.into(),
            }],
        };
        let held = [values(&[("API_KEY", "key"), ("GITHUB_TOKEN", "token")])];
        assert_eq!(
            places(&plan(&[&file], &held, &NOTHING).expect("planned")),
            [
                "GITHUB_TOKEN: the GitHub login default",
                "API_KEY: the secret api-key, for example.com",
            ]
        );
    }

    #[test]
    fn a_pulled_secret_the_person_has_no_key_for_stops_them_only_when_it_is_required() {
        // An optional key left blank, or not in the file at all.
        let optional = pulled(vec![secret("aivar-api-key", &["api.aivar.no"], true)]);
        for held in [values(&[("AIVAR_API_KEY", "")]), values(&[])] {
            assert!(plan(&[&optional], &[held], &NOTHING).is_ok());
        }
        let required = pulled(vec![secret("aivar-api-key", &["api.aivar.no"], false)]);
        assert!(plan(&[&required], &[values(&[])], &NOTHING).is_err());

        // One left blank that leaves the hosts to the stored secret.
        let blank = pulled(vec![secret("aivar-api-key", &[], true)]);
        assert!(plan(&[&blank], &[values(&[("AIVAR_API_KEY", "")])], &NOTHING).is_ok());

        // A migrated entry that leaves the hosts to the stored secret: by hand, then done.
        let without_hosts = pulled(vec![secret("aivar-api-key", &[], false)]);
        let held = [values(&[("AIVAR_API_KEY", "key")])];
        assert!(
            plan(&[&without_hosts], &held, &NOTHING).is_err(),
            "it cannot tell the hosts"
        );
        assert!(
            plan(&[&without_hosts], &held, &kept(&["aivar-api-key"])).is_ok(),
            "stored by hand"
        );
    }

    #[test]
    fn what_else_the_files_hold_is_named_before_it_goes_with_them() {
        let variables = [
            variable("GITHUB_TOKEN", Some(&["api.github.com"])),
            variable("GIT_USER_NAME", None),
            renamed("API_KEY", "KEY", Some(&["example.com"])),
        ];
        let mut unreadable = read(&variables);
        unreadable.readers.push(Reader::Unreadable {
            name: "agent.mine.yaml".into(),
        });
        let others = |file: &EnvironmentFile, held: &[BTreeMap<String, Zeroizing<String>>]| {
            plan(&[file], held, &NOTHING).expect("planned").others
        };
        let held = [values(&[
            ("GITHUB_TOKEN", "t"),
            ("GIT_USER_NAME", "Alice"),
            ("KEY", "k"),
            ("OLD", "o"),
        ])];

        // Git's name and email go with the login, so they are not among the others.
        let line = others(&unreadable, &held).expect("named");
        assert!(
            line.starts_with("The files also hold OLD, which only agent.mine.yaml")
                && line.contains("deleted with the files and not stored"),
            "{line}"
        );
        let line = others(&read(&variables), &held).expect("named");
        assert!(line.contains("OLD, which nothing here takes"), "{line}");

        let held = [values(&[
            ("GITHUB_TOKEN", "t"),
            ("GIT_USER_NAME", "Alice"),
            ("KEY", "k"),
        ])];
        assert_eq!(others(&read(&variables), &held), None);
        assert!(others(&unreadable, &held).is_some_and(|line| line.contains("nothing else it could take")));
    }
}
