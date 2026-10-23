//! `agentctl repos`: shows and changes the repositories in an agent folder's `repositories.yaml`,
//! which agentd hands to the running agent on its next setup pass.

use std::path::{Path, PathBuf};

use agent::{
    Error,
    control_api::Client,
    repositories::{self, REPOSITORIES_FILE, Repository},
};

use crate::CommandResult;

/// The folder whose repositories `agentctl repos` changes, with its agent's name when one was
/// created from it: the current directory's agent's, or else the current directory when it is an
/// agent folder.
async fn target(client: &Client, agent: Option<&str>) -> CommandResult<(PathBuf, Option<String>)> {
    if let Some((folder, (name, _manifest))) = crate::tools::agent_folder(client, agent).await? {
        return Ok((folder, Some(name)));
    }
    let directory = std::env::current_dir().map_err(Error::from)?;
    if directory.join(REPOSITORIES_FILE).is_file() || directory.join(agent::manifest::MANIFEST_FILE).is_file() {
        return Ok((directory, None));
    }
    Err(Error::Invalid("no agent here; `agentctl init` sets one up, or pass --agent NAME".into()).into())
}

/// What to change in the list.
pub(crate) enum Change {
    Add {
        repo: String,
        path: Option<String>,
        about: Option<String>,
    },
    Remove {
        repo: String,
    },
}

/// `agentctl repos`: shows the repositories, after adding or removing one.
///
/// # Errors
///
/// Returns an error when no agent folder is found, or the file or the change is not valid.
pub(crate) async fn run(client: &Client, agent: Option<&str>, change: Option<Change>) -> CommandResult<()> {
    let (folder, agent) = target(client, agent).await?;
    let mut list = repositories::read(&folder)?;
    if let Some(change) = change {
        println!("{}", apply(&mut list, change, agent.as_deref())?);
        write(&folder, &list)?;
        println!();
    }
    if list.is_empty() {
        println!("No repositories; `agentctl repos add owner/name` adds one.");
    }
    for repository in &list {
        let about = repository
            .about
            .as_deref()
            .map_or_else(String::new, |about| format!(": {about}"));
        println!("  {} in {}{about}", repository.repo, repository.destination());
    }
    Ok(())
}

/// Applies `change` to `list`, returning what to tell the person.
fn apply(list: &mut Vec<Repository>, change: Change, agent: Option<&str>) -> Result<String, Error> {
    let who = agent.unwrap_or("The agent");
    match change {
        Change::Add { repo, path, about } => {
            if list.iter().any(|existing| existing.repo.eq_ignore_ascii_case(&repo)) {
                return Err(Error::Invalid(format!("{repo} is already in {REPOSITORIES_FILE}")));
            }
            list.push(Repository { repo, path, about });
            let added = &list[list.len() - 1];
            Ok(format!(
                "Added {}. {who} clones it into {} within a minute while it runs.",
                added.repo,
                added.destination()
            ))
        }
        Change::Remove { repo } => {
            let index = list
                .iter()
                .position(|existing| existing.repo.eq_ignore_ascii_case(&repo))
                .ok_or_else(|| Error::Invalid(format!("{repo} is not in {REPOSITORIES_FILE}")))?;
            let removed = list.remove(index);
            Ok(format!(
                "Removed {}. {who} keeps its clone in {}; delete it there when you no longer need it.",
                removed.repo,
                removed.destination()
            ))
        }
    }
}

/// Writes `list` to `folder`'s `repositories.yaml`, unless agentd would refuse what it reads back.
fn write(folder: &Path, list: &[Repository]) -> Result<(), Error> {
    let text = repositories::render(list);
    repositories::parse(&text)?;
    std::fs::write(folder.join(REPOSITORIES_FILE), text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn add(repo: &str, path: Option<&str>) -> Change {
        Change::Add {
            repo: repo.into(),
            path: path.map(Into::into),
            about: None,
        }
    }

    #[test]
    fn adding_and_removing_say_what_the_agent_does_with_its_clone() {
        let mut list = Vec::new();
        let added = apply(&mut list, add("digdir/app", None), Some("app")).expect("added");
        assert_eq!(
            added,
            "Added digdir/app. app clones it into app within a minute while it runs."
        );
        let duplicate = apply(&mut list, add("Digdir/App", Some("other")), Some("app")).expect_err("duplicate");
        assert!(duplicate.to_string().contains("already"), "{duplicate}");
        let removed = apply(
            &mut list,
            Change::Remove {
                repo: "digdir/app".into(),
            },
            None,
        )
        .expect("removed");
        assert!(
            removed.starts_with("Removed digdir/app. The agent keeps its clone in app"),
            "{removed}"
        );
        assert!(list.is_empty());
    }
}
