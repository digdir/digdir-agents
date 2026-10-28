//! Connecting the new agent to an editor installed here, over the SSH alias `agentctl
//! ssh-config install` makes OpenSSH resolve. VS Code and Zed are opened in the agent's working
//! directory. Apps that keep SSH connections in their own settings, such as JetBrains IDEs and the
//! Claude desktop app, connect to the same alias, as the README describes.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use agent::{control_api::Client, ssh::AccessInfo};
use clap::ValueEnum;

use super::answers::{Answers, Choice};
use crate::{CommandResult, launch::Editor};

/// VS Code's extension for working in a folder over SSH.
const VS_CODE_REMOTE_SSH: &str = "ms-vscode-remote.remote-ssh";

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum IdeChoice {
    /// Visual Studio Code, with its Remote-SSH extension.
    Vscode,
    /// Zed.
    Zed,
    /// Connect nothing.
    None,
}

impl IdeChoice {
    /// Its spelling as the value of `--ide`.
    const fn name(self) -> &'static str {
        match self {
            Self::Vscode => "vscode",
            Self::Zed => "zed",
            Self::None => "none",
        }
    }

    const fn of(editor: Editor) -> Self {
        match editor {
            Editor::VsCode => Self::Vscode,
            Editor::Zed => Self::Zed,
        }
    }
}

/// Offers the editors found here and opens the chosen one in the agent `name`. Whichever is
/// chosen, it ends by naming the SSH host and folder any other app connects to.
pub(super) async fn offer(
    answers: &Answers,
    choice: Option<IdeChoice>,
    client: &Client,
    name: &str,
) -> CommandResult<()> {
    let path = std::env::var_os("PATH");
    let found: Vec<(Editor, PathBuf)> = Editor::ALL
        .into_iter()
        .filter_map(|editor| Some((editor, editor.locate(path.as_deref())?)))
        .collect();
    let editor = match choice {
        Some(IdeChoice::None) => None,
        Some(choice) => Editor::ALL
            .into_iter()
            .find(|editor| IdeChoice::of(*editor) == choice)
            .map(|editor| {
                (
                    editor,
                    found
                        .iter()
                        .find(|(found, _)| *found == editor)
                        .map(|(_, path)| path.clone()),
                )
            }),
        None if found.is_empty() || !answers.interactive() => None,
        None => {
            let mut choices: Vec<_> = found
                .iter()
                .map(|(editor, path)| {
                    Choice::new(
                        Some((*editor, Some(path.clone()))),
                        format!("Open it in {}", editor.label()),
                    )
                    .flag("--ide", &[IdeChoice::of(*editor).name()])
                })
                .collect();
            choices.push(Choice::new(None, "Not now").flag("--ide", &[IdeChoice::None.name()]));
            answers.choose("--ide", "Open the agent in an editor on this machine?", choices, 0)?
        }
    };
    let access = client.ssh_access(name).await?;
    if let Some((editor, launcher)) = editor {
        open_editor(editor, launcher.as_deref(), &access).await;
    }
    println!(
        "Other apps, such as a JetBrains IDE or the Claude desktop app, connect over SSH to host {} and open {}.",
        access.alias, access.working_directory
    );
    Ok(())
}

async fn open_editor(editor: Editor, launcher: Option<&Path>, access: &AccessInfo) {
    if editor == Editor::VsCode
        && let Some(code) = launcher
    {
        install_vs_code_extension(code).await;
    }
    let Some(launch) = editor.launch(launcher, &access.alias, &access.working_directory) else {
        eprintln!(
            "{} cannot be opened: {}",
            editor.label(),
            editor.missing_launcher().unwrap_or_default()
        );
        return;
    };
    match launch.start().await {
        Ok(()) => println!("Opened {} in {}.", access.working_directory, editor.label()),
        Err(error) => eprintln!("{} did not open: {error}", editor.label()),
    }
}

/// Installs Remote-SSH, which VS Code needs to open a folder over SSH. Already installed, VS Code
/// says so and changes nothing; a failure leaves VS Code to offer it when the folder opens.
async fn install_vs_code_extension(code: &Path) {
    let installing = tokio::process::Command::new(code)
        .args(["--install-extension", VS_CODE_REMOTE_SSH])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match tokio::time::timeout(Duration::from_secs(120), installing).await {
        Ok(Ok(status)) if status.success() => {}
        _ => eprintln!("warning: could not install VS Code's {VS_CODE_REMOTE_SSH}; VS Code offers it when it opens"),
    }
}
