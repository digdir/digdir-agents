//! The Agent sandbox section `agentd` writes ahead of an Agent's own instructions.
//!
//! Every Agent runs on the same platform, so what a harness needs to know about it is generated
//! here, from the Agent's manifest and what setup found in its Sandbox, rather than copied into
//! each Agent's instruction file, where it drifts from the release that runs it. It states facts,
//! then the platform's ground rules, one labelled line each, with a sub-bullet per item where a
//! line lists several; how to drive Agents, nested ones included, is the `agentctl` skill's.

use std::{collections::BTreeMap, fmt::Write as _};

use crate::{
    Harness,
    control_plane::AgentRecord,
    github::GithubToken,
    manifest::{AccessSpec, MountSpec, Spec},
};

use super::{HOME, USER, WORKING_DIRECTORY};

/// What setup found in the Sandbox, beyond the manifest.
pub(super) struct Observed<'a> {
    /// The harnesses preparation installed, which leaves out an optional one without a host login.
    pub(super) harnesses: &'a [Harness],
    /// The Sandbox's environment, holding the configured secrets' placeholders.
    pub(super) environment: &'a BTreeMap<String, String>,
    /// Whether the guest has `/dev/kvm`, which nested Sandboxes need.
    pub(super) kvm: bool,
    /// Whether the image has Podman, which builds the images of nested Agents.
    pub(super) podman: bool,
    /// The GitHub token stored under the name the Agent's account asks for, without the token.
    pub(super) github_token: Option<&'a GithubToken>,
}

/// Opens the section, so that a later pass can find and replace it in a file the image or the home
/// overlay owns.
const BEGIN: &str = "<!-- agentd: Agent sandbox section, rewritten on every pass -->\n";
/// Closes the section.
const END: &str = "<!-- agentd: end of Agent sandbox section -->\n";

/// The instruction file: the section, then `rest`, separated by a blank line.
pub(super) fn place(section: &str, rest: &[u8]) -> Vec<u8> {
    let mut contents = section.as_bytes().to_vec();
    if !rest.is_empty() {
        contents.push(b'\n');
        contents.extend_from_slice(rest);
    }
    contents
}

/// What an instruction file holds besides a section an earlier pass placed at its top.
pub(super) fn without_section(existing: &[u8]) -> &[u8] {
    existing
        .strip_prefix(BEGIN.as_bytes())
        .and_then(|after| {
            let end = after.windows(END.len()).position(|window| window == END.as_bytes())?;
            let rest = &after[end + END.len()..];
            Some(rest.strip_prefix(b"\n").unwrap_or(rest))
        })
        .unwrap_or(existing)
}

/// Renders the section for `record`, between its markers and ending in a newline. A line with
/// nothing to say is left out.
pub(super) fn render(record: &AgentRecord, observed: &Observed<'_>) -> String {
    let spec = &record.agent.spec;
    let name = &record.agent.metadata.name;
    let lines = [
        (
            "Computer",
            Some(format!(
                "Agent `{name}`, a Linux Sandbox VM that is yours to manage. User `{USER}` with passwordless \
                 `sudo`, home `{HOME}`, work in `{WORKING_DIRECTORY}`."
            )),
        ),
        ("Resources", Some(resources(spec))),
        ("Harnesses", Some(harnesses(spec, observed.harnesses))),
        ("Folders", folders(spec)),
        ("Secrets", secrets(spec, observed.environment)),
        (
            "Plain environment",
            (!spec.environment.is_empty())
                .then(|| format!("{}.", code_list(spec.environment.iter().map(|variable| &variable.name)))),
        ),
        ("Network", Some(network(spec))),
        ("GitHub", github(spec, observed.github_token)),
        ("Access", access(spec, name)),
        ("Nested Agents", Some(nested_agents(spec, observed).to_owned())),
        ("Changes", Some(CHANGES.to_owned())),
        ("Cleanup", Some(CLEANUP.to_owned())),
        ("Tools and repositories", Some(TOOLS_AND_REPOSITORIES.to_owned())),
    ];
    let mut section = format!(
        "{BEGIN}# Agent sandbox\n\n\
         Written by agentd {}. Instructions after this section are the Agent's own and take precedence.\n\n",
        crate::build_version()
    );
    for (label, text) in lines {
        if let Some(text) = text {
            // Text that is only sub-bullets starts on the next line.
            let separator = if text.starts_with('\n') { "" } else { " " };
            let _ = writeln!(section, "- **{label}:**{separator}{text}");
        }
    }
    section.push_str(END);
    section
}

/// What the manifest gives the Sandbox. Disk is what nested Agents and image builds run out of first.
fn resources(spec: &Spec) -> String {
    let resources = &spec.sandbox.resources;
    format!(
        "cpu {}, memory {}, disk {}; `df -h /` shows how much is free.",
        resources.cpu,
        resources.memory,
        resources.root_filesystem.capacity()
    )
}

/// One sub-bullet per declared harness, naming the default and those not installed.
fn harnesses(spec: &Spec, installed: &[Harness]) -> String {
    sub_bullets(spec.harnesses.iter().map(|installation| {
        let label = installation.kind.display_name();
        if !installed.contains(&installation.kind) {
            format!("{label}: not installed until the host logs in to it")
        } else if installation.default {
            format!("{label} (default)")
        } else {
            label.to_owned()
        }
    }))
}

/// One sub-bullet per folder mounted from the person's machine, read-only ones marked; nothing
/// without any.
fn folders(spec: &Spec) -> Option<String> {
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
    (!folders.is_empty()).then(|| {
        format!(
            "the person's own files, mounted from their machine: changes happen at once and outlive this Agent, \
             so don't switch branches, reset or push there unless asked.{}",
            sub_bullets(folders.into_iter())
        )
    })
}

/// How GitHub works for an Agent with an account: as the account itself when its token reaches
/// all the account can, else for the repositories the token was created for, as a fine-grained
/// token is.
fn github(spec: &Spec, token: Option<&GithubToken>) -> Option<String> {
    let name = spec.github_account()?;
    let reach = match token {
        Some(token) if !token.kind.single_owner() => {
            format!("as `{}`, for whatever that account can reach", token.account.login)
        }
        _ => format!("for the repositories the `{name}` token was created for"),
    };
    Some(format!(
        "`gh` and `git` over HTTPS work {reach}, and SSH remotes on github.com go over HTTPS; \
         `gh api user --jq .login` shows the account. Don't run `gh auth login` or replace `GITHUB_TOKEN`."
    ))
}

/// What the Agent may do without asking, what only when a task asks, and where it notes system changes.
const CHANGES: &str = "inside this VM, apart from the person's folders, act freely; outside it, such as pushes, \
                       pull requests and comments, act only when a task asks. Note what you change in the system, and \
                       why, in `~/system-changes.md`, so it can be moved into the image.";

/// What the Agent removes, so its disk keeps room for later tasks.
const CLEANUP: &str = "stop the servers and containers you start. When a task is done, remove what you created for \
                       it, such as one-off images, finished worktrees and build output, and nothing else.";

/// What the Agent does with the tools and repositories its own instructions list.
const TOOLS_AND_REPOSITORIES: &str = "at the start of a session, check for those your instructions list; install \
                                      or clone what is missing, at the versions listed, and say what you installed. \
                                      In clones, fetch first, branch from the default branch, never discard work you \
                                      didn't make, and push what should outlive this Agent.";

/// One sub-bullet per secret, with the hosts it is sent to or that it was left unconfigured.
fn secrets(spec: &Spec, environment: &BTreeMap<String, String>) -> Option<String> {
    if spec.secrets.is_empty() {
        return None;
    }
    let mut text = String::from(
        "inert placeholders the host replaces only in requests to their hosts; never search for, print or \
         persist real values.",
    );
    text.push_str(&sub_bullets(spec.secrets.iter().map(|secret| {
        if environment.contains_key(&secret.environment) {
            format!("`{}`: {}", secret.environment, secret.allowed_hosts.join(", "))
        } else {
            format!("`{}`: not configured", secret.environment)
        }
    })));
    Some(text)
}

/// Items as sub-bullets of the line they follow.
fn sub_bullets(items: impl Iterator<Item = String>) -> String {
    items.fold(String::new(), |mut text, item| {
        let _ = write!(text, "\n  - {item}");
        text
    })
}

fn network(spec: &Spec) -> String {
    let mut text = String::from(
        "all traffic passes through the host, which intercepts TLS with a CA that the system store and \
         `NODE_EXTRA_CA_CERTS` trust.",
    );
    if !spec.network.deny.is_empty() {
        let _ = write!(text, " Denied: {}.", code_list(&spec.network.deny));
    }
    text
}

fn access(spec: &Spec, name: &str) -> Option<String> {
    let commands = spec
        .access
        .iter()
        .map(|access| match access {
            AccessSpec::Ssh {} => format!("`agentctl ssh {name}`"),
            AccessSpec::Vnc {} => format!("`agentctl vnc {name}`"),
        })
        .collect::<Vec<_>>();
    (!commands.is_empty()).then(|| {
        format!(
            "people on the host reach this computer with {}.",
            commands.join(" and ")
        )
    })
}

const fn nested_agents(spec: &Spec, observed: &Observed<'_>) -> &'static str {
    match (observed.kvm, observed.podman) {
        (true, true) => "available (`/dev/kvm` and Podman); see the `agentctl` skill.",
        (true, false) => {
            "available (`/dev/kvm`, no Podman, so only manifests with an image `reference`); see the `agentctl` skill."
        }
        (false, _) if spec.sandbox.resources.nested_sandboxes.is_some() => {
            "unavailable: the host cannot provide `/dev/kvm`."
        }
        (false, _) => "unavailable: the manifest does not request nested Sandboxes.",
    }
}

fn code_list<'a>(items: impl IntoIterator<Item = &'a String>) -> String {
    items
        .into_iter()
        .map(|item| format!("`{item}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn spec(mounts: &str, extra: &str) -> Spec {
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
    fn only_bind_mounts_are_folders_and_read_only_ones_are_marked() {
        assert_eq!(
            folders(&spec("[{type: tmpfs, target: /scratch, capacity: 1Gi}]", "")),
            None
        );
        let text = folders(&spec(
            "[{type: bind, source: /tmp, target: /home/agent/code/app, readOnly: false}, \
             {type: bind, source: /tmp, target: /home/agent/code/reports, readOnly: true}]",
            "",
        ))
        .expect("folders");
        assert!(
            text.ends_with("unless asked.\n  - `/home/agent/code/app`\n  - `/home/agent/code/reports` (read-only)"),
            "{text}"
        );
    }

    #[test]
    fn github_works_as_the_account_unless_the_token_is_for_one_owner() {
        assert_eq!(github(&spec("[]", ""), None), None);
        let spec = spec("[]", "  accounts: [{type: github, name: altinn-studio}]\n");
        let token = |kind| GithubToken {
            name: "altinn-studio".into(),
            account: crate::github::GithubAccount {
                login: "digdir-agent".into(),
                name: "Digdir Agent".into(),
                email: "1+digdir-agent@users.noreply.github.com".into(),
            },
            kind,
            expires_at: None,
        };
        let repositories =
            "`gh` and `git` over HTTPS work for the repositories the `altinn-studio` token was created for,";
        let account = "`gh` and `git` over HTTPS work as `digdir-agent`, for whatever that account can reach,";
        for (token, expected) in [
            (None, repositories),
            (Some(token(crate::github::TokenKind::FineGrained)), repositories),
            (Some(token(crate::github::TokenKind::Oauth)), account),
            (Some(token(crate::github::TokenKind::Classic)), account),
        ] {
            let text = github(&spec, token.as_ref()).expect("a GitHub line");
            assert!(text.starts_with(expected), "{expected} in {text}");
            assert!(text.contains("`gh api user --jq .login`"), "{text}");
        }
    }
}
