//! Editing the person's variant before it is applied, for everything the questions leave at its
//! recommended value, such as an exact size, the model or hosts the agent may not reach.

use std::process::Command;

use agent::{Error, manifest};

use super::{Draft, answers::Answers, create};
use crate::CommandResult;

/// Opens the person's variant, beside the manifest it extends, in `$VISUAL` or `$EDITOR` until it
/// resolves to a valid agent, or the person gives up and keeps the one from before.
pub(super) fn variant(answers: &Answers, draft: &mut Draft) -> CommandResult<()> {
    let path = draft.source.variant.clone();
    if !draft.changing {
        create::ensure_ignored(answers, &path)?;
    }
    let before = draft.agent.clone();
    if draft.edited.is_none() {
        draft.edited = Some(std::fs::read(&path).map_or(Before::Absent, Before::Held));
    }
    std::fs::write(&path, create::variant_text(draft, &path)?).map_err(Error::from)?;
    loop {
        open_editor(&path)?;
        match manifest::resolve(&path) {
            Ok(resolved) => {
                draft.agent = resolved.agent;
                return Ok(());
            }
            Err(error) => {
                eprintln!("The variant does not resolve: {error}");
                if !answers.confirm("--edit", &["Edit it again? No keeps the agent from before."], true)? {
                    draft.agent = before;
                    std::fs::write(&path, create::variant_text(draft, &path)?).map_err(Error::from)?;
                    return Ok(());
                }
            }
        }
    }
}

/// The person's variant before they edited it here.
pub(super) enum Before {
    Absent,
    Held(Vec<u8>),
}

/// Puts the person's variant back as it was before they edited it here, as when they decide to
/// write nothing: removed if there was none.
pub(super) fn undo(draft: &Draft) {
    let path = &draft.source.variant;
    match &draft.edited {
        Some(Before::Held(contents)) => {
            std::fs::write(path, contents).ok();
        }
        Some(Before::Absent) => {
            std::fs::remove_file(path).ok();
        }
        None => {}
    }
}

/// The editor the person chose with `$VISUAL` or `$EDITOR`, else the system's own.
pub(super) fn editor() -> String {
    ["VISUAL", "EDITOR"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.trim().is_empty()))
        .unwrap_or_else(|| if cfg!(windows) { "notepad".into() } else { "vi".into() })
}

/// Opens `path` in [`editor`] and waits until it is closed.
pub(super) fn open_editor(path: &std::path::Path) -> Result<(), Error> {
    let editor = editor();
    // The variable may carry arguments, such as `code --wait`.
    let mut words = editor.split_whitespace();
    let program = words.next().unwrap_or("vi");
    let status = Command::new(program)
        .args(words)
        .arg(path)
        .status()
        .map_err(|error| Error::Invalid(format!("could not start the editor {program}: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Invalid(format!("the editor {program} exited with {status}")))
    }
}
