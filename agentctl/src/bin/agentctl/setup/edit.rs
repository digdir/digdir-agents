//! Editing an agent's whole manifest before it is created, for everything the questions leave at
//! its recommended value, such as an exact size, the model or hosts the agent may not reach.

use std::process::Command;

use agent::{Error, manifest};

use super::{Draft, answers::Answers};

/// Opens the manifest in `$VISUAL` or `$EDITOR` until it is a valid agent, or the person gives up
/// and keeps the one from before.
pub(super) fn manifest(answers: &Answers, draft: &mut Draft) -> Result<(), Error> {
    let path = draft.source.directory.join("agent.setup-edit.yaml");
    let mut text = serde_yaml_ng::to_string(&draft.agent).map_err(Error::Yaml)?;
    loop {
        std::fs::write(&path, &text)?;
        let edited = open_editor(&path);
        text = std::fs::read_to_string(&path)?;
        std::fs::remove_file(&path).ok();
        edited?;
        // Paths resolve against the manifest's folder, as the written manifest's will.
        match manifest::decode(text.as_bytes()).and_then(|agent| agent.validate().map(|()| agent)) {
            Ok(agent) => {
                draft.agent = agent;
                return Ok(());
            }
            Err(error) => {
                eprintln!("The manifest is not valid: {error}");
                if !answers.confirm("--edit", &["Edit it again? No keeps the manifest from before."], true)? {
                    return Ok(());
                }
            }
        }
    }
}

fn open_editor(path: &std::path::Path) -> Result<(), Error> {
    let editor = ["VISUAL", "EDITOR"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.trim().is_empty()))
        .unwrap_or_else(|| if cfg!(windows) { "notepad".into() } else { "vi".into() });
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
