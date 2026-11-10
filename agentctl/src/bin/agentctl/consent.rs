//! Asking the person before agentd mounts or copies a folder with files that may hold real keys,
//! such as a checkout's `.env`.

use std::{collections::BTreeSet, path::PathBuf};

use agent::{Error, exposed::ExposedFile};

use crate::prompt;

/// How a command asks about the files an agent could read.
pub(crate) struct Ask<'a> {
    /// Who would read them, such as `agent/reviewer`, starting a sentence.
    pub(crate) reader: &'a str,
    /// What agentd does with them once the person agrees, such as `Mount`.
    pub(crate) action: &'a str,
    /// Whether `--allow-env-files` agreed to them already.
    pub(crate) allowed: bool,
}

/// Sends `request` with `send` and, when agentd names files that may hold real keys, asks about them
/// and sends it again naming them in `allowed`, the request's list of files the person agreed to.
/// agentd checks again, so a file that appeared in the meantime is asked about too.
///
/// # Errors
///
/// Returns the error `send` returns, or why the files are not let through: the person said no,
/// nobody is at the terminal to ask, or one is an agent's environment file that stays.
pub(crate) async fn send<R, T, F>(
    mut request: R,
    ask: &Ask<'_>,
    allowed: impl Fn(&mut R) -> &mut BTreeSet<PathBuf>,
    send: impl Fn(R) -> F,
) -> Result<T, Error>
where
    R: Clone,
    F: Future<Output = Result<T, Error>>,
{
    loop {
        match send(request.clone()).await {
            Err(Error::ExposedFiles(files)) => {
                let agreed = agree(&files, ask)?;
                // Fewer are agreed to than agentd named once an environment file was deleted.
                if agreed.len() == files.len() && agreed.is_subset(allowed(&mut request)) {
                    // agentd refuses what was agreed to, which asking again would not change.
                    return Err(Error::ExposedFiles(files));
                }
                *allowed(&mut request) = agreed;
            }
            result => return result,
        }
    }
}

/// The paths of `files` once the person agrees to them, or why not. An agent's environment file is
/// never agreed to. One that nothing reads any more is deleted instead, once the person agrees to
/// that, and is then not among the paths. Every question is asked before a file is deleted, so a no
/// to any of them leaves everything as it was.
pub(crate) fn agree(files: &[ExposedFile], ask: &Ask<'_>) -> Result<BTreeSet<PathBuf>, Error> {
    // The person knows the files by their paths from the folder they work in.
    let from = crate::builder::folder(None).ok();
    let (environment, files): (Vec<_>, Vec<_>) =
        files.iter().cloned().partition(|file| file.environment_file.is_some());
    let unread = unread(&environment, from.as_deref())?;
    for file in &unread {
        if !prompt::interactive() {
            return Err(Error::Invalid(agent::exposed::refusal_from(
                &environment,
                from.as_deref(),
            )));
        }
        let lines = deletion_question(file, from.as_deref());
        if !prompt::confirm_lines(&lines.iter().map(String::as_str).collect::<Vec<_>>())? {
            return Err(Error::Invalid("nothing was changed".into()));
        }
    }
    let agreed = if files.is_empty() {
        BTreeSet::new()
    } else {
        agree_to_read(&files, ask)?
    };
    let mut deleted = Vec::new();
    for file in unread {
        let shown = file.shown(from.as_deref());
        if let Err(error) = std::fs::remove_file(&file.path) {
            let before = if deleted.is_empty() {
                String::new()
            } else {
                format!("deleted {}, but ", agent::exposed::sentence_list(&deleted))
            };
            return Err(Error::Invalid(format!("{before}could not delete {shown}: {error}")));
        }
        deleted.push(shown);
    }
    Ok(agreed)
}

/// The paths of `files`, which may hold real keys and are no agent's environment file, once the
/// person agrees to let the agent read them, or why not.
fn agree_to_read(files: &[ExposedFile], ask: &Ask<'_>) -> Result<BTreeSet<PathBuf>, Error> {
    let paths = files.iter().map(|file| file.path.clone()).collect();
    let listed = listed(files);
    if ask.allowed {
        // Said where a script's output is kept, as no question records it.
        eprintln!(
            "{} will be able to read {}{}, as --allow-env-files lets it",
            ask.reader,
            agent::exposed::sentence_list(&listed),
            within(files),
        );
        return Ok(paths);
    }
    if !prompt::interactive() {
        return Err(Error::Invalid(format!(
            "{} would be able to read {}{}, which may hold real keys, and the agent can reach the internet. \
             Pass --allow-env-files to {} them anyway",
            ask.reader,
            agent::exposed::sentence_list(&listed),
            within(files),
            ask.action.to_lowercase(),
        )));
    }
    let lines = question(ask, files, &listed);
    if prompt::confirm_lines(&lines.iter().map(String::as_str).collect::<Vec<_>>())? {
        Ok(paths)
    } else {
        Err(Error::Invalid("nothing was changed".into()))
    }
}

/// Why agentctl never lets an agent read some of `files`, when they are agents' environment files,
/// whose values belong in stored secrets instead, and what to do about each.
pub(crate) fn environment_file_refusal(files: &[ExposedFile]) -> Option<String> {
    files
        .iter()
        .any(|file| file.environment_file.is_some())
        .then(|| agent::exposed::refusal_from(files, crate::builder::folder(None).ok().as_deref()))
}

/// The `environment` files to offer to delete, each once though several of an agent's folders hold
/// it.
///
/// # Errors
///
/// Returns what to do about them instead when something still reads one, as deleting it would lose
/// the values.
fn unread<'a>(environment: &'a [ExposedFile], from: Option<&std::path::Path>) -> Result<Vec<&'a ExposedFile>, Error> {
    let read = |file: &ExposedFile| {
        file.environment_file
            .as_ref()
            .is_some_and(|file| !file.readers.is_empty())
    };
    if environment.iter().any(read) {
        return Err(Error::Invalid(agent::exposed::refusal_from(environment, from)));
    }
    let mut seen = BTreeSet::new();
    Ok(environment.iter().filter(|file| seen.insert(&file.path)).collect())
}

/// The question about deleting the environment file `file`, which nothing reads any more: what the
/// person must have stored first, as deleting it loses the values, then the question.
fn deletion_question(file: &ExposedFile, from: Option<&std::path::Path>) -> Vec<String> {
    let mut lines = vec![
        format!("{}:", agent::exposed::UNREAD),
        format!("  {}", file.shown(from)),
    ];
    lines.extend(
        file.environment_file
            .iter()
            .flat_map(agent::exposed::EnvironmentFile::stored_lines),
    );
    lines.push("Delete the file?".into());
    lines
}

/// The question about `files`, `listed` as [`listed`] gives them: who would read them, each file on
/// a line of its own, then the risk and the question.
fn question(ask: &Ask<'_>, files: &[ExposedFile], listed: &[String]) -> Vec<String> {
    let mut lines = vec![format!(
        "{} will be able to read these files{}:",
        ask.reader,
        within(files)
    )];
    lines.extend(listed.iter().map(|file| format!("  {file}")));
    lines.push(format!(
        "They may hold real keys, and the agent can reach the internet. {} anyway?",
        ask.action
    ));
    lines
}

/// Each of `files` as the person knows it: within its folder when they share one, else in full.
pub(crate) fn listed(files: &[ExposedFile]) -> Vec<String> {
    let shared = one_folder(files).is_some();
    files
        .iter()
        .map(|file| if shared { file.relative() } else { &file.path }.display().to_string())
        .collect()
}

/// The folder all of `files` are in, as ` in FOLDER`, when they share one, as [`listed`] names them within it.
fn within(files: &[ExposedFile]) -> String {
    one_folder(files).map_or_else(String::new, |folder| format!(" in {}", folder.display()))
}

fn one_folder(files: &[ExposedFile]) -> Option<&std::path::Path> {
    let folder = &files.first()?.folder;
    files
        .iter()
        .all(|file| file.folder == *folder)
        .then_some(folder.as_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(folder: &str, relative: &str, environment_file: bool) -> ExposedFile {
        ExposedFile {
            path: PathBuf::from(folder).join(relative),
            folder: PathBuf::from(folder),
            target: None,
            environment_file: environment_file.then(agent::exposed::EnvironmentFile::default),
        }
    }

    const ASK: Ask<'static> = Ask {
        reader: "agent/builder",
        action: "Mount",
        allowed: false,
    };

    #[test]
    fn files_in_one_folder_are_listed_within_it_and_others_in_full() {
        let files = [
            file("/src/app", "src/web/.env", false),
            file("/src/app", "tools/.env", false),
        ];
        let lines = question(&ASK, &files, &listed(&files));
        assert_eq!(lines[0], "agent/builder will be able to read these files in /src/app:");
        assert_eq!(&lines[1..3], ["  src/web/.env", "  tools/.env"]);

        let files = [file("/src/app", ".env", false), file("/src/tools", ".env", false)];
        assert_eq!(
            listed(&files),
            files.map(|file| file.path.display().to_string()),
            "in full"
        );
    }

    #[test]
    fn an_environment_file_is_never_agreed_to_even_with_the_flag() {
        let files = [
            file("/src/app", "agents/reviewer/.env", true),
            file("/src/app", ".env", false),
        ];
        let ask = Ask { allowed: true, ..ASK };
        let refused = agree(&files, &ask).expect_err("refused").to_string();
        assert!(
            refused.contains("agents/reviewer/.env") && !refused.contains("/src/app/.env"),
            "{refused}"
        );
        assert_eq!(
            agree(&files[1..], &ask).expect("agreed"),
            BTreeSet::from([PathBuf::from("/src/app/.env")])
        );
    }

    #[test]
    fn an_environment_file_that_nothing_reads_is_only_deleted_by_a_person_at_the_terminal() {
        assert!(!prompt::interactive(), "tests run without a terminal");
        let directory = tempfile::tempdir().expect("directory");
        let folder = directory.path().display().to_string();
        std::fs::write(directory.path().join(".env"), "KEY=1").expect("file");
        let mut left = file(&folder, ".env", true);
        left.environment_file = Some(agent::exposed::EnvironmentFile {
            readers: Vec::new(),
            stored: vec![agent::exposed::Stored::GithubLogin { login: "work".into() }],
        });

        let lines = deletion_question(&left, Some(directory.path()));
        assert_eq!(
            lines[1], "  .env",
            "named from where the person works, on a line of its own"
        );
        assert!(
            lines.iter().any(|line| line.contains("agentctl github login work")),
            "what to store before it goes: {lines:?}"
        );
        agree(&[left], &Ask { allowed: true, ..ASK }).expect_err("nobody to ask");
        assert!(directory.path().join(".env").exists());
    }

    #[test]
    fn an_environment_file_is_offered_for_deletion_once_and_never_while_something_reads_it() {
        // The builder is given a checkout and the folder of personal agents, which may hold it.
        let twice = [
            file("/home/me/agents/dev", ".env", true),
            file("/home/me/agents", "dev/.env", true),
        ];
        let offered = unread(&twice, None).expect("nothing reads it");
        assert_eq!(offered.len(), 1, "one file, in two folders");

        let mut read = file("/src/app", "agents/dev/.env", true);
        read.environment_file = Some(agent::exposed::EnvironmentFile {
            readers: vec![agent::exposed::Reader::Unreadable {
                name: "agent.yaml".into(),
            }],
            stored: Vec::new(),
        });
        let files = [file("/src/app", "agents/old/.env", true), read];
        let refused = unread(&files, None).expect_err("one is still read").to_string();
        assert!(
            refused.contains("agents/dev/.env") && refused.contains("agent.yaml"),
            "{refused}"
        );
    }

    #[test]
    fn without_a_terminal_or_the_flag_nothing_is_agreed_to() {
        assert!(!prompt::interactive(), "tests run without a terminal");
        let refused = agree(&[file("/src/app", ".env", false)], &ASK).expect_err("refused");
        assert!(refused.to_string().contains("--allow-env-files"));
    }
}
