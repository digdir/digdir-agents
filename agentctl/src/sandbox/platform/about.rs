//! The platform's own instructions, "About this sandbox": what every agent should know about the
//! sandbox it runs in, installed ahead of the host-wide and the agent's own instructions.
//!
//! Only what applies to the agent is said, so an agent without bind mounts hears nothing about them.

use std::fmt::Write as _;

use crate::manifest::MountSpec;

/// The platform's instructions for an agent with `spec` and its stored GitHub `token`.
pub(super) fn instructions(spec: &crate::Spec, token: Option<&crate::github::GithubToken>) -> String {
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
agent, so be as careful as you would be on their laptop: don't switch branches, reset or push there unless they \
ask.",
            folders.join(", ")
        );
    }
    text.push_str(
        "
- **Network and credentials.** Your traffic goes through the person's machine. Credentials in your environment are \
placeholders the host swaps for the real values on the way out, so authentication works; don't report them as broken \
or replace them.
",
    );
    if let Some(owner) = spec.github_owner() {
        // Only a fine-grained token is limited to its owner; any other reaches all its account can.
        let reach = match token {
            Some(token) if !token.kind.single_owner() => {
                format!("as `{}`, for whatever that account can reach", token.account.login)
            }
            _ => format!("for `{owner}`'s repositories"),
        };
        let _ = writeln!(
            text,
            "- **GitHub.** `gh` and `git` over HTTPS already work with GitHub {reach}; SSH remotes on github.com go \
over HTTPS. Don't run `gh auth login` or replace `GITHUB_TOKEN`."
        );
    }
    text.push_str(
        "- **Settings live in environment variables.** Check `printenv` before asking for a value.
- **Tools and repositories.** Your instructions may list tools and repositories you should have. At the start of a \
session, check that they are there. Install or clone what is missing, at the versions listed, and tell the person what \
you installed. What you install outside the image is gone when the agent is recreated, so suggest adding it to the \
agent's image if it should stay. In repositories you clone, fetch before starting work, branch from the remote's \
default branch, never reset or discard work you didn't make, and push what should outlive the agent.
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
        spec_with(mounts, "")
    }

    fn spec_with(mounts: &str, extra: &str) -> crate::Spec {
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
{extra}"
        );
        crate::manifest::decode(manifest.as_bytes()).expect("manifest").spec
    }

    #[test]
    fn an_agent_without_bind_mounts_hears_nothing_about_folders() {
        let text = instructions(&spec("[{type: tmpfs, target: /scratch, capacity: 1Gi}]"), None);
        assert!(!text.contains("Folders"), "{text}");
        assert!(!text.contains("/scratch"), "{text}");
        assert!(!text.contains("GitHub"), "{text}");
        assert!(text.starts_with("# About this sandbox\n\n"), "{text}");
        for part in [
            "Network and credentials",
            "printenv",
            "Tools and repositories",
            "suggest adding it to the agent's image if it should stay",
            "In repositories you clone, fetch before starting work",
            "ask the person.",
        ] {
            assert!(text.contains(part), "{part} in {text}");
        }
    }

    #[test]
    fn bind_mounts_are_named_with_the_read_only_ones_marked() {
        let text = instructions(
            &spec(
                "[{type: bind, source: /tmp, target: /home/agent/code/app, readOnly: false}, \
{type: bind, source: /tmp, target: /home/agent/code/reports, readOnly: true}]",
            ),
            None,
        );
        assert!(
            text.contains(
                "- **Folders from the person's machine:** `/home/agent/code/app`, `/home/agent/code/reports` \
(read-only). These are their own files"
            ),
            "{text}"
        );
        assert!(
            text.contains("don't switch branches, reset or push there unless they ask."),
            "the clone rules are not for the person's own checkout: {text}"
        );
    }

    #[test]
    fn an_agent_with_a_github_account_hears_how_to_use_it() {
        let spec = spec_with("[]", "  accounts: [{type: github, owner: Digdir}]\n");
        let token = |kind| crate::github::GithubToken {
            owner: "digdir".into(),
            account: crate::github::GithubAccount {
                login: "digdir-agent".into(),
                name: "Digdir Agent".into(),
                email: "1+digdir-agent@users.noreply.github.com".into(),
            },
            kind,
            expires_at: None,
        };
        let line = |reach: &str| {
            format!(
                "\n- **GitHub.** `gh` and `git` over HTTPS already work with GitHub {reach}; SSH remotes on github.com \
go over HTTPS. Don't run `gh auth login` or replace `GITHUB_TOKEN`.\n- **Settings live"
            )
        };
        let owners = line("for `Digdir`'s repositories");
        let account = line("as `digdir-agent`, for whatever that account can reach");
        for (token, expected) in [
            (None, &owners),
            (Some(token(crate::github::TokenKind::FineGrained)), &owners),
            (Some(token(crate::github::TokenKind::Oauth)), &account),
            (Some(token(crate::github::TokenKind::Classic)), &account),
        ] {
            let text = instructions(&spec, token.as_ref());
            assert!(text.contains(expected.as_str()), "{expected} in {text}");
        }
    }
}
