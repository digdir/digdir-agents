//! `agentctl guide`: what a coding agent needs to set up and check agents, built into this binary
//! so it always matches the installed version. Topics are the markdown files in `agentctl/guide/`.

use std::fmt::Write as _;

use crate::{CommandError, CommandResult};

/// Each topic's name, what it covers, and its text.
const TOPICS: [(&str, &str, &str); 6] = [
    (
        "setup",
        "Set up an agent with a person: what to find out, write, start and check",
        include_str!("../../../guide/setup.md"),
    ),
    (
        "manifest",
        "agent.yaml: a complete, annotated example, variants, .env and secrets",
        concat!(
            include_str!("../../../guide/manifest.md"),
            "```yaml\n",
            include_str!("../../../guide/agent.yaml"),
            "```\n"
        ),
    ),
    (
        "image",
        "The agent's Dockerfile on the agentctl base images",
        include_str!("../../../guide/image.md"),
    ),
    (
        "instructions",
        "The agent's instructions: what agentctl already says, and what to write",
        include_str!("../../../guide/instructions.md"),
    ),
    (
        "accounts",
        "GitHub accounts: what they give an agent, and the login the person runs",
        include_str!("../../../guide/accounts.md"),
    ),
    (
        "status",
        "The object `agentctl status --json` prints",
        include_str!("../../../guide/status.md"),
    ),
];

const INTRO: &str = "agentctl runs coding agents in sandboxed Linux VMs on this machine. An agent is declared by a
manifest, agent.yaml, beside a Dockerfile for its image and its instructions; agentd, which agentctl starts
when needed, builds and runs it.

These topics describe this version of agentctl. Read one with `agentctl guide <topic>`:

";

/// Prints the topic, or without one the index. A reader that stops early, such as `head`, is no error.
pub(crate) fn print(topic: Option<&str>) -> CommandResult<()> {
    let text = text(topic)?;
    match std::io::Write::write_all(&mut std::io::stdout().lock(), text.as_bytes()) {
        Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => Err(agent::Error::from(error).into()),
        _ => Ok(()),
    }
}

/// The text `agentctl guide [topic]` prints: the topic, or without one the index.
pub(crate) fn text(topic: Option<&str>) -> CommandResult<String> {
    let Some(topic) = topic else {
        let mut index = INTRO.to_owned();
        for (name, summary, _) in TOPICS {
            let _ = writeln!(index, "  {name:<14}{summary}");
        }
        return Ok(index);
    };
    TOPICS
        .iter()
        .find(|(name, ..)| *name == topic)
        .map(|(.., text)| (*text).to_owned())
        .ok_or_else(|| {
            let names = TOPICS.map(|(name, ..)| name).join(", ");
            CommandError::Message(format!("no guide topic {topic:?}; the topics are {names}"))
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn guide_directory() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("guide")
    }

    #[test]
    fn the_index_lists_every_topic_and_every_topic_is_a_file_in_the_guide() {
        let index = text(None).expect("index");
        let mut files = std::fs::read_dir(guide_directory())
            .expect("guide directory")
            .map(|entry| entry.expect("guide entry").path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
            .map(|path| path.file_stem().expect("stem").to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        files.sort();
        let mut topics = TOPICS.map(|(name, ..)| name.to_owned()).to_vec();
        topics.sort();
        assert_eq!(files, topics, "every guide/*.md is a topic, and every topic a file");
        for (name, summary, _) in TOPICS {
            assert!(index.contains(&format!("  {name:<14}{summary}\n")), "{index}");
            let topic = text(Some(name)).expect("topic");
            assert!(topic.starts_with("# "), "{name} starts with its title");
            assert!(topic.ends_with('\n'), "{name} ends with a newline");
        }
    }

    #[test]
    fn an_unknown_topic_names_the_topics() {
        let Err(CommandError::Message(message)) = text(Some("manifests")) else {
            panic!("an unknown topic is an error");
        };
        assert_eq!(
            message,
            "no guide topic \"manifests\"; the topics are setup, manifest, image, instructions, accounts, status"
        );
    }

    #[test]
    fn the_manifest_topic_shows_the_example_manifest_and_it_is_valid() {
        let example = include_str!("../../../guide/agent.yaml");
        let topic = text(Some("manifest")).expect("manifest topic");
        assert!(topic.ends_with(&format!("```yaml\n{example}```\n")), "{topic}");
        let agent = agent::manifest::decode(example.as_bytes()).expect("the example manifest is valid");
        assert_eq!(agent.metadata.name, "dev");
    }

    #[test]
    fn the_variant_in_the_manifest_topic_extends_the_example() {
        let topic = include_str!("../../../guide/manifest.md");
        let variant = topic
            .split_once("```yaml\n")
            .and_then(|(_, rest)| rest.split_once("```\n"))
            .map(|(variant, _)| variant)
            .expect("a variant example");
        let directory = tempfile::tempdir().expect("temporary directory");
        std::fs::write(
            directory.path().join("agent.yaml"),
            include_str!("../../../guide/agent.yaml"),
        )
        .expect("write the example");
        std::fs::write(directory.path().join("agent.small.yaml"), variant).expect("write the variant");
        let resolved =
            agent::manifest::resolve(&directory.path().join("agent.small.yaml")).expect("the variant resolves");
        assert_eq!(resolved.agent.metadata.name, "dev-small");
        assert_eq!(resolved.agent.spec.sandbox.resources.cpu.to_string(), "2");
    }
}
