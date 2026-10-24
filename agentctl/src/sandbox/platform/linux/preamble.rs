//! The Agent sandbox section `agentd` writes ahead of an Agent's own instructions.
//!
//! Every Agent runs on the same platform, so what a harness needs to know about it is generated
//! here, from the Agent's manifest and what setup found in its Sandbox, rather than copied into
//! each Agent's instruction file, where it drifts from the release that runs it. It states facts
//! only, one labelled line each, with a sub-bullet per item where a line lists several; how to drive
//! Agents, nested ones included, is the `agentctl` skill's.

use std::{collections::BTreeMap, fmt::Write as _};

use crate::{
    Harness,
    control_plane::AgentRecord,
    manifest::{AccessSpec, Spec},
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
}

/// Renders the section for `record`, ending in a newline. A line with nothing to say is left out.
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
        ("Harnesses", Some(harnesses(spec, observed.harnesses))),
        ("Secrets", secrets(spec, observed.environment)),
        (
            "Plain environment",
            (!spec.environment.is_empty())
                .then(|| format!("{}.", code_list(spec.environment.iter().map(|variable| &variable.name)))),
        ),
        ("Network", Some(network(spec))),
        ("Access", access(spec, name)),
        ("Nested Agents", Some(nested_agents(spec, observed).to_owned())),
    ];
    let mut section = format!(
        "# Agent sandbox\n\n\
         Written by agentd {}. The Agent's own instructions follow and take precedence.\n\n",
        crate::build_version()
    );
    for (label, text) in lines {
        if let Some(text) = text {
            // Text that is only sub-bullets starts on the next line.
            let separator = if text.starts_with('\n') { "" } else { " " };
            let _ = writeln!(section, "- **{label}:**{separator}{text}");
        }
    }
    section
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
        (true, true) => "available: `/dev/kvm` and Podman are present. Use the `agentctl` skill.",
        (true, false) => {
            "available, but without Podman only for manifests with an image `reference`. Use the `agentctl` skill."
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
