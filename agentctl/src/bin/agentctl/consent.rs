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
/// nobody is at the terminal to ask, or one is an agent's environment file.
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
                if agreed.is_subset(allowed(&mut request)) {
                    // agentd refuses what was agreed to, which asking again would not change.
                    return Err(Error::ExposedFiles(files));
                }
                *allowed(&mut request) = agreed;
            }
            result => return result,
        }
    }
}

/// The paths of `files` once the person agrees to them, or why not.
pub(crate) fn agree(files: &[ExposedFile], ask: &Ask<'_>) -> Result<BTreeSet<PathBuf>, Error> {
    if let Some(refusal) = environment_file_refusal(files) {
        return Err(Error::Invalid(refusal));
    }
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
/// whose values belong in stored secrets instead.
pub(crate) fn environment_file_refusal(files: &[ExposedFile]) -> Option<String> {
    files
        .iter()
        .any(|file| file.environment_file)
        .then(|| agent::exposed::refusal(files))
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
            environment_file,
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
        let refused = agree(&files, &ask).expect_err("refused");
        assert!(refused.to_string().contains(agent::exposed::ENVIRONMENT_FILE_GUIDE));
        assert_eq!(
            agree(&files[1..], &ask).expect("agreed"),
            BTreeSet::from([PathBuf::from("/src/app/.env")])
        );
    }

    #[test]
    fn without_a_terminal_or_the_flag_nothing_is_agreed_to() {
        assert!(!prompt::interactive(), "tests run without a terminal");
        let refused = agree(&[file("/src/app", ".env", false)], &ASK).expect_err("refused");
        assert!(refused.to_string().contains("--allow-env-files"));
    }
}
