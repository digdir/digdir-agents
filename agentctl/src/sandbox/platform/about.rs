//! The platform's own instructions, "About this sandbox": what every agent should know about the
//! sandbox it runs in, installed ahead of the host-wide and the agent's own instructions.
//!
//! Only what applies to the agent is said, so an agent without bind mounts hears nothing about them.

use std::fmt::Write as _;

use crate::manifest::MountSpec;

/// The platform's instructions for an agent with `spec`.
pub(super) fn instructions(spec: &crate::Spec) -> String {
    let mut text = String::from(
        "# About this sandbox

You run in an isolated Linux VM that agentctl manages for the person you work with. Files you create here last until \
the agent is recreated.
",
    );
    let folders: Vec<String> = spec
        .sandbox
        .mounts
        .iter()
        .filter_map(|mount| match mount {
            MountSpec::Bind { target, read_only, .. } => Some(if *read_only {
                format!("`{}` (read-only)", target.as_str())
            } else {
                format!("`{}`", target.as_str())
            }),
            MountSpec::Tmpfs { .. } => None,
        })
        .collect();
    if !folders.is_empty() {
        let _ = write!(
            text,
            "
- **Folders from the person's machine:** {}. These are their own files: changes happen at once and outlive the \
agent, so be as careful as you would be on their laptop.",
            folders.join(", ")
        );
    }
    text.push_str(
        "
- **Network and credentials.** Your traffic goes through the person's machine. Credentials in your environment are \
placeholders the host swaps for the real values on the way out, so authentication works; don't report them as broken \
or replace them.
- **Settings live in environment variables.** Check `printenv` before asking for a value.
- **Tools and repositories.** Your instructions may list tools and repositories you should have. At the start of a \
session, check that they are there. Install or clone what is missing, at the versions listed, and tell the person what \
you installed. What you install outside the image is gone when the agent is recreated, so suggest adding it to the \
agent's image if it should stay. In repositories, fetch before starting work, branch from the remote's default \
branch, never reset or discard work you didn't make, and push what should outlive the agent.
- **Your setup is declared on the person's machine.** For a lasting change to your folders, size or image, ask the \
person.
",
    );
    text
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn spec(mounts: &str) -> crate::Spec {
        let manifest = format!(
            "apiVersion: agents.platform/v1alpha1
kind: Agent
metadata:
  name: worker
spec:
  sandbox:
    image:
      type: reference
      reference: example.com/agent:latest
    platform:
      os: linux
    resources:
      cpu: \"1\"
      memory: \"1Gi\"
      rootFilesystem:
        capacity: \"8Gi\"
        mode: layered
    mounts: {mounts}
  harnesses: [{{type: pi}}]
"
        );
        crate::manifest::decode(manifest.as_bytes()).expect("manifest").spec
    }

    #[test]
    fn an_agent_without_bind_mounts_hears_nothing_about_folders() {
        let text = instructions(&spec("[{type: tmpfs, target: /scratch, capacity: 1Gi}]"));
        assert!(!text.contains("Folders"), "{text}");
        assert!(!text.contains("/scratch"), "{text}");
        assert!(text.starts_with("# About this sandbox\n\n"), "{text}");
        for part in [
            "Network and credentials",
            "printenv",
            "Tools and repositories",
            "suggest adding it to the agent's image if it should stay",
            "ask the person.",
        ] {
            assert!(text.contains(part), "{part} in {text}");
        }
    }

    #[test]
    fn bind_mounts_are_named_with_the_read_only_ones_marked() {
        let text = instructions(&spec(
            "[{type: bind, source: /tmp, target: /home/agent/code/app, readOnly: false}, \
{type: bind, source: /tmp, target: /home/agent/code/reports, readOnly: true}]",
        ));
        assert!(
            text.contains(
                "- **Folders from the person's machine:** `/home/agent/code/app`, `/home/agent/code/reports` \
(read-only). These are their own files"
            ),
            "{text}"
        );
    }
}
