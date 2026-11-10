//! `agentctl new`: making a new agent with the agent builder, in its `new-agent` session.

use std::path::PathBuf;

use agent::{control_api::Client, local::home::ControlPlaneHome};
use clap::Args;

use crate::{
    CommandError, CommandResult,
    builder::{self, Builder, Task},
};

#[derive(Args)]
pub(crate) struct NewArguments {
    /// The checkout of the repository the new agent is for. Defaults to the current folder.
    directory: Option<PathBuf>,
    /// Delete the agent builder when this command ends, such as when you leave its session,
    /// instead of keeping it to go back to. What it wrote stays.
    #[arg(long)]
    rm: bool,
}

/// Makes a new agent with the builder, starting it on the checkout or going back to it there.
pub(crate) async fn new(arguments: NewArguments, home: &ControlPlaneHome, client: &Client) -> CommandResult<()> {
    if !crate::prompt::interactive() {
        return Err(CommandError::Message(
            "making a new agent is a conversation with the agent builder, which needs a terminal".into(),
        ));
    }
    let checkout = builder::folder(arguments.directory)?;
    let builder = Builder::bundled()?;
    // The builder makes one agent at a time.
    let task = Task {
        session: "new-agent",
        prompt: "I want to make a new agent for the repository you have checked out.".into(),
        manifests: agent::exposed::Manifests::ByHand,
    };
    let name = match builder.find(client, &checkout).await? {
        Some(existing) if arguments.rm => return Err(builder::kept_already("new", &checkout, &existing)),
        Some(existing) => {
            let name = existing.metadata.name.clone();
            builder.resume(home, client, &existing, &checkout, &task).await?;
            if !builder.current(&existing) {
                eprintln!(
                    "This agent builder was started by another version of agentctl, and keeps its image. To make \
                     agents with this version's, run `agentctl delete agent/{name}` and `agentctl new` again. What it \
                     wrote stays where it is."
                );
            }
            name
        }
        None => {
            builder
                .start(
                    home,
                    client,
                    &checkout,
                    &task,
                    std::collections::BTreeSet::new(),
                    arguments.rm,
                )
                .await?
        }
    };
    if arguments.rm {
        println!(
            "The agent builder is deleted. Apply the agent it made with `agentctl apply -f` and the manifest it named."
        );
    } else {
        println!(
            "The agent builder keeps running, and `agentctl new` in {} goes back to it.\nApply the agent it made \
             with `agentctl apply -f` and the manifest it named. Run `agentctl delete agent/{name}` once you are done with \
             it.",
            checkout.display()
        );
    }
    Ok(())
}
