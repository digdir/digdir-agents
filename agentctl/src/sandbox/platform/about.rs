//! The platform's own instructions: what every agent should know about the sandbox it runs in,
//! installed ahead of the host-wide and the agent's own instructions.
//!
//! Each part describes only what this agent has, from its manifest and its folder's
//! `repositories.yaml`, so an agent that works on mounted folders hears nothing about Git.

use std::fmt::Write as _;

use crate::{manifest::MountSpec, repositories::Repository};

use super::linux::WORKING_DIRECTORY;

/// The instructions for an agent with `spec` and `repositories`.
pub(super) fn instructions(spec: &crate::Spec, repositories: &[Repository]) -> String {
    let mut text = String::from(
        "# About this sandbox

You run in an isolated Linux VM that agentctl manages for the person you work with. Files you create in it last until \
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
- **Folders from the person's machine:** {}. Changes there are changes to their own files: they happen at once and \
outlive the agent, so be as careful as you would be on their laptop.",
            folders.join(", ")
        );
    }
    if !repositories.is_empty() {
        let cloned: Vec<String> = repositories
            .iter()
            .map(|repository| {
                let about = repository
                    .about
                    .as_deref()
                    .map_or_else(String::new, |about| format!(": {about}"));
                format!(
                    "`{}` in `{WORKING_DIRECTORY}/{}`{about}",
                    repository.repo,
                    repository.destination()
                )
            })
            .collect();
        let _ = write!(
            text,
            "
- **Repositories cloned for you:** {}. They are never updated for you. Fetch before starting work, branch from the \
remote's default branch, never reset or discard work you didn't make, and push what should outlive the agent.",
            cloned.join("; ")
        );
    }
    if spec.github_account() {
        text.push_str("\n- `gh` and `git` over HTTPS already work with GitHub.");
    }
    text.push_str(
        "
- **Network and credentials.** Your traffic goes through the person's machine. Credentials in your environment are \
placeholders, such as `…-agent-mediated-placeholder-not-a-real-credential`. The host swaps in the real value on the \
way out, so authentication works; don't report them as broken or try to replace them.
- **Settings live in environment variables.** Check `printenv` before asking for a value.
- **Your setup is declared on the person's machine.** A tool you install yourself is gone when the agent is recreated. \
If you need one permanently, or another folder or repository, ask the person to add it.
",
    );
    text
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn spec(mounts: &str, accounts: &str) -> crate::Spec {
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
  accounts: {accounts}
"
        );
        crate::manifest::decode(manifest.as_bytes()).expect("manifest").spec
    }

    #[test]
    fn an_agent_on_mounted_folders_hears_nothing_about_git() {
        let text = instructions(
            &spec(
                "[{type: bind, source: /tmp, target: /home/agent/code/reports, readOnly: true}]",
                "[]",
            ),
            &[],
        );
        assert!(text.contains("`/home/agent/code/reports` (read-only)"), "{text}");
        for absent in ["git", "Repositories", "GitHub"] {
            assert!(!text.contains(absent), "{absent} in {text}");
        }
    }

    #[test]
    fn cloned_repositories_come_with_what_they_are_for_and_the_git_rules() {
        let repositories = [
            Repository {
                repo: "digdir/app".into(),
                path: None,
                about: Some("The app itself.".into()),
            },
            Repository {
                repo: "digdir/microsandbox".into(),
                path: Some("forks/microsandbox".into()),
                about: None,
            },
        ];
        let text = instructions(&spec("[]", "[{type: github}]"), &repositories);
        assert!(
            text.contains(
                "`digdir/app` in `/home/agent/code/app`: The app itself.; `digdir/microsandbox` in \
`/home/agent/code/forks/microsandbox`."
            ),
            "{text}"
        );
        assert!(text.contains("Fetch before starting work"), "{text}");
        assert!(text.contains("`gh` and `git` over HTTPS"), "{text}");
        assert!(!text.contains("Folders from"), "{text}");
    }
}
