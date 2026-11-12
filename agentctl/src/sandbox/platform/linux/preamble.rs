//! The agent sandbox section `agentd` writes ahead of an agent's own instructions.
//!
//! Every agent runs on the same platform, so what a harness needs to know about it is generated
//! here, from the agent's manifest and what setup found in its sandbox, rather than copied into
//! each agent's instruction file, where it drifts from the release that runs it. It states facts,
//! then the platform's ground rules, one labelled line each, with a sub-bullet per item where a
//! line lists several; how to drive agents, nested ones included, is the `agentctl` skill's.

use std::{collections::BTreeMap, fmt::Write as _};

use ::sandbox::mount::Mount;

use crate::{
    Harness,
    control_plane::AgentRecord,
    manifest::{AccessSpec, NetworkAllow, NetworkSpec, Spec},
};

use super::{HOME, USER, WORKING_DIRECTORY};

/// What setup found in the sandbox, beyond the manifest.
pub(super) struct Observed<'a> {
    /// The harnesses preparation installed, which leaves out an optional one without a host login.
    pub(super) harnesses: &'a [Harness],
    /// The sandbox's environment, holding the configured secrets' placeholders.
    pub(super) environment: &'a BTreeMap<String, String>,
    /// Whether the guest has `/dev/kvm`, which nested sandboxes need.
    pub(super) kvm: bool,
    /// Whether the image has Podman, which builds the images of nested agents.
    pub(super) podman: bool,
    /// What setup configures for the accounts the agent acts as, such as lines for these instructions.
    pub(super) accounts: &'a crate::sandbox::AccountSetup,
}

/// Opens the section, so that a later pass can find and replace it in a file the image or the home
/// overlay owns.
const BEGIN: &str = "<!-- agentd: Agent sandbox section, rewritten on every pass -->\n";
/// Closes the section.
const END: &str = "<!-- agentd: end of Agent sandbox section -->\n";

/// Opens the agent's own instructions below the section. The rest of the file is then setup's, so a
/// later pass without instructions of the agent's own replaces it rather than keeping it as the
/// image's or the home overlay's.
const OWN: &str = "<!-- agentd: the agent's own instructions, from spec.instructions -->\n";

/// The instruction file: the section, then `rest`, separated by a blank line.
pub(super) fn place(section: &str, rest: &[u8]) -> Vec<u8> {
    let mut contents = section.as_bytes().to_vec();
    if !rest.is_empty() {
        contents.push(b'\n');
        contents.extend_from_slice(rest);
    }
    contents
}

/// The instruction file for an agent with instructions of its own: the section, then `own`.
pub(super) fn place_own(section: &str, own: &[u8]) -> Vec<u8> {
    let mut rest = OWN.as_bytes().to_vec();
    rest.extend_from_slice(own);
    place(section, &rest)
}

/// What an instruction file holds besides what an earlier pass placed: the section at its top, and
/// the agent's own instructions below it.
pub(super) fn without_section(existing: &[u8]) -> &[u8] {
    existing
        .strip_prefix(BEGIN.as_bytes())
        .and_then(|after| {
            let end = after.windows(END.len()).position(|window| window == END.as_bytes())?;
            let rest = &after[end + END.len()..];
            let rest = rest.strip_prefix(b"\n").unwrap_or(rest);
            Some(if rest.starts_with(OWN.as_bytes()) { &[] } else { rest })
        })
        .unwrap_or(existing)
}

/// Renders the section for `record`, between its markers and ending in a newline. A line with
/// nothing to say is left out.
pub(super) fn render(record: &AgentRecord, observed: &Observed<'_>) -> String {
    let spec = &record.agent.spec;
    let name = &record.agent.metadata.name;
    let mut lines = vec![
        (
            "Computer",
            Some(format!(
                "agent `{name}`, a Linux sandbox VM that is yours to manage. User `{USER}` with passwordless \
                 `sudo`, home `{HOME}`, work in `{WORKING_DIRECTORY}`. What you send through the host, such as \
                 pushes, pull requests, comments and API calls, reaches real services and people."
            )),
        ),
        ("Resources", Some(resources(spec))),
        ("Harnesses", harnesses(spec, observed.harnesses)),
        ("Folders", folders(&record.sandbox_mounts())),
        ("Secrets", secrets(spec, observed.environment)),
        (
            "Plain environment",
            (!spec.environment.is_empty())
                .then(|| format!("{}.", code_list(spec.environment.iter().map(|variable| &variable.name)))),
        ),
        ("Network", Some(network(&spec.network))),
    ];
    lines.extend(
        observed
            .accounts
            .instructions
            .iter()
            .map(|(label, text)| (*label, Some(text.clone()))),
    );
    lines.extend([
        ("Access", access(spec, name)),
        ("Nested agents", Some(nested_agents(spec, observed).to_owned())),
        ("Changes", Some(CHANGES.to_owned())),
        ("Cleanup", Some(CLEANUP.to_owned())),
        ("Clones", Some(CLONES.to_owned())),
    ]);
    let mut section = format!(
        "{BEGIN}# Agent sandbox\n\n\
         Written by agentd {}. Instructions after this section are the agent's own and take precedence.\n\n",
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

/// What the manifest gives the sandbox. Disk is what nested agents and image builds run out of first.
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
fn harnesses(spec: &Spec, installed: &[Harness]) -> Option<String> {
    if spec.harnesses.is_empty() {
        return None;
    }
    Some(sub_bullets(spec.harnesses.iter().map(|installation| {
        let label = installation.kind.display_name();
        if !installed.contains(&installation.kind) {
            format!("{label}: not installed until the host logs in to it")
        } else if installation.default {
            format!("{label} (default)")
        } else {
            label.to_owned()
        }
    })))
}

/// One sub-bullet per folder mounted from the person's machine, read-only ones marked; nothing
/// without any. A volume is left out: it is a copy, so the agent's changes to it stay in the agent.
fn folders(mounts: &[Mount]) -> Option<String> {
    let folders: Vec<String> = mounts
        .iter()
        .filter_map(|mount| match mount {
            Mount::Bind {
                target,
                read_only: true,
                ..
            } => Some(format!("`{}` (read-only)", target.as_str())),
            Mount::Bind { target, .. } => Some(format!("`{}`", target.as_str())),
            Mount::Volume { .. } | Mount::Tmpfs { .. } => None,
        })
        .collect();
    (!folders.is_empty()).then(|| {
        format!(
            "the person's own files, mounted from their machine: changes happen at once and outlive this agent, \
             so don't switch branches, reset or push there unless asked.{}",
            sub_bullets(folders.into_iter())
        )
    })
}

/// What the agent may do without asking, and where it notes what the image should have had.
const CHANGES: &str = "inside this VM, apart from the person's folders, act freely, such as installing the tools \
                       a task needs. Send something out through the host only when a task asks. Note what you \
                       install or change in the system, and why, in `~/system-changes.md`, so it can be moved \
                       into the image.";

/// What the agent removes, so its disk keeps room for later tasks.
const CLEANUP: &str = "stop the servers and containers you start. When a task is done, remove what you created for \
                       it, such as one-off images, finished worktrees and build output, and nothing else.";

/// How the agent treats a repository it works in, so that it neither builds on stale code nor loses anyone's work.
const CLONES: &str = "fetch first, branch from the default branch, and never discard work you didn't make.";

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
        if !environment.contains_key(&secret.environment) {
            format!("`{}`: not configured", secret.environment)
        } else if let (true, Some(name)) = (secret.allowed_hosts.is_empty(), &secret.name) {
            format!("`{}`: the hosts its stored secret {name} allows", secret.environment)
        } else {
            format!("`{}`: {}", secret.environment, secret.allowed_hosts.join(", "))
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

fn network(network: &NetworkSpec) -> String {
    let mut text = String::from(
        "all traffic passes through the host, which intercepts TLS with a CA that the system store and \
         `NODE_EXTRA_CA_CERTS` trust.",
    );
    match &network.allow {
        NetworkAllow::All => {}
        NetworkAllow::Hosts(hosts) if hosts.is_empty() => text.push_str(" No other host is allowed."),
        NetworkAllow::Hosts(hosts) => {
            let _ = write!(
                text,
                " Allowed: only {}, by name through this computer's DNS, since the host checks that it resolved \
                 the name to the address dialed.",
                code_list(hosts)
            );
        }
    }
    if !network.deny.is_empty() {
        let _ = write!(text, " Denied: {}.", code_list(&network.deny));
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
        (false, _) => "unavailable: the manifest does not request nested sandboxes.",
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
    use super::*;

    #[test]
    fn storage_the_sandbox_owns_is_no_folder_of_the_persons() {
        let mounts = [
            Mount::Volume {
                id: "5c0fd6ac-1d5a-4f8b-9f58-6b0f4de1f6c1".parse().expect("volume ID"),
                target: ::sandbox::SandboxPath::new("/home/agent/reference"),
                read_only: false,
            },
            Mount::Tmpfs {
                target: ::sandbox::SandboxPath::new("/scratch"),
                capacity: "1Gi".parse().expect("capacity"),
            },
        ];
        assert_eq!(folders(&mounts), None);
    }

    #[test]
    fn the_network_line_names_the_allowed_and_denied_hosts() {
        let trust = "all traffic passes through the host, which intercepts TLS with a CA that the system store and \
                     `NODE_EXTRA_CA_CERTS` trust.";
        let mut spec = NetworkSpec {
            deny: vec!["metadata.google.internal".into()],
            ..NetworkSpec::default()
        };
        assert_eq!(network(&spec), format!("{trust} Denied: `metadata.google.internal`."));

        spec.allow = NetworkAllow::Hosts(vec!["github.com".into(), "*.github.com".into()]);
        assert_eq!(
            network(&spec),
            format!(
                "{trust} Allowed: only `github.com`, `*.github.com`, by name through this computer's DNS, since the \
                 host checks that it resolved the name to the address dialed. Denied: `metadata.google.internal`."
            )
        );

        spec.allow = NetworkAllow::Hosts(Vec::new());
        spec.deny.clear();
        assert_eq!(network(&spec), format!("{trust} No other host is allowed."));
    }
}
