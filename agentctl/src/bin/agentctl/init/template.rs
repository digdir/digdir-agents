//! The files `agentctl init` writes into `agents/<name>/`: the manifest, a variant working on
//! the checkout, the image's Dockerfile, instructions, a home overlay and an `.env` sample.

use agent::harness::AuthenticationStatus;

use super::{image::Tier, sizing::Size, toolchains::Toolchain};

/// The directory under the repository root that holds one directory per agent.
pub(super) const AGENTS_DIRECTORY: &str = "agents";
/// The variant of a cloning agent that works on the checkout instead.
pub(super) const WORKTREE_VARIANT: &str = "agent.worktree.yaml";
/// The scratch space a build gets in `/tmp`; the Sandbox default is a quarter of guest memory,
/// capped at 512 MiB, too small for a large build. A tmpfs holds only what is written to it.
const TMP_CAPACITY: &str = "2Gi";

/// Whether a new manifest gives the Agent a GitHub token, and whose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Github {
    /// No token applies; the manifest shows how to add one.
    None,
    /// The only stored token, whichever owner it is for.
    Only,
    /// The token for this owner, or the one for every organization without its own.
    Owner(String),
}

/// Where `agent.yaml`'s copy of the repository comes from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Checkout {
    /// A clone made at the agent's first boot, from `owner/name` on GitHub or a Git URL;
    /// [`WORKTREE_VARIANT`] mounts the checkout instead.
    Clone(String),
    /// This checkout, mounted read-write into the agent.
    Mount,
}

/// Everything the person chose for a new agent.
#[derive(Clone, Debug)]
pub(super) struct Choices {
    pub(super) name: String,
    /// The directory under the agent's working directory that holds the repository.
    pub(super) workspace: String,
    pub(super) github: Github,
    pub(super) tier: Tier,
    pub(super) checkout: Checkout,
    pub(super) size: Size,
    /// The repository's own skill directories, relative to its root.
    pub(super) skills: Vec<String>,
}

/// `agent.yaml` for `choices`, with every setting a person is likely to change later present,
/// and those that do not apply yet commented out. Every harness is listed, so a later login adds
/// it on the agent's next pass.
pub(super) fn manifest(choices: &Choices, status: &AuthenticationStatus) -> String {
    let Choices {
        name,
        workspace,
        github,
        tier,
        checkout,
        size,
        skills,
    } = choices;
    let code = agent::sandbox::platform::WORKING_DIRECTORY;
    let harnesses = harnesses(status);
    let accounts = accounts(github);
    let access = if tier.has_desktop() {
        "    # Opens the image's desktop to `agentctl vnc` and the TUI's open menu.\n    - type: vnc\n"
    } else {
        ""
    };
    let tmp = tmp_mount(if tier.has_browser() {
        "Build scratch space and the browser's shared memory, held in memory only as it is used."
    } else {
        "Build scratch space, held in memory only as it is used."
    });
    let (about, checkout) = match checkout {
        Checkout::Clone(repository) => (
            format!(
                "# The agent clones {repository} into {code}/{workspace} at first boot.\n# {WORKTREE_VARIANT} is the same agent working on this checkout instead.\n"
            ),
            String::new(),
        ),
        Checkout::Mount => (
            format!(
                "# The agent works on this checkout, mounted at {code}/{workspace}: what it changes there\n# changes on this machine.\n"
            ),
            bind_mount(workspace),
        ),
    };
    let skills = skills_section(skills);
    let Size {
        cpus,
        memory_gib,
        disk_gib,
    } = size;
    format!(
        "# Created by `agentctl init`. Change it and run `agentctl apply` to update the agent. The image
# (including the Dockerfile's contents), mounts and harness types stay as the agent was created: to
# change them, delete the agent and create it again, which starts its disk over.
{about}apiVersion: agents.platform/v1alpha1
kind: Agent
metadata:
  name: {name}
spec:
  sandbox:
    # Built with Docker on this machine from Dockerfile.
    image:
      type: build
      context: .
      dockerfile: Dockerfile
    platform:
      os: linux
    resources:
      cpu: \"{cpus}\"
      memory: \"{memory_gib}Gi\"
      # The most the agent's disk may grow to. It takes space on this machine only as the agent
      # writes, so keep it below the free space here.
      rootFilesystem:
        capacity: \"{disk_gib}Gi\"
        mode: layered
    initSystem: image
    mounts:
{checkout}{tmp}  # Copied into the agent's home directory on every pass, such as dotfiles.
  home:
    source: home
  # Installed for every harness as its own CLAUDE.md or AGENTS.md; the checkout's still apply.
  instructions:
    - source: instructions.md
{skills}  harnesses:
{harnesses}{accounts}  # Plain values from .env (see .env.sample), visible in the agent.
  # environment:
  #   - name: GIT_USER_NAME
  #   - name: GIT_USER_EMAIL
  # Secrets from .env: the agent sees a placeholder, which this machine swaps for the value only
  # in requests to the allowed hosts.
  # secrets:
  #   - environment: AZURE_DEVOPS_PAT
  #     allowedHosts:
  #       - dev.azure.com
  access:
    - type: ssh
{access}  # All traffic goes through this machine's network mediator, and every host is allowed. To block
  # some:
  # network:
  #   mode: mediated
  #   allow: all
  #   deny:
  #     - metadata.google.internal
"
    )
}

/// Every harness, required when signed in and optional otherwise, with Claude Code the default
/// unless only Codex is signed in.
fn harnesses(status: &AuthenticationStatus) -> String {
    let claude_default = status.claude_code || !status.codex;
    let harness = |kind: &str, signed_in: bool, default: bool| {
        let default = if default { "      default: true\n" } else { "" };
        let optional = if signed_in {
            String::new()
        } else {
            format!(
                "      # Optional until `agentctl {} login` stores a login on this host.\n      optional: true\n",
                if kind == "codex" { "codex" } else { "claude" }
            )
        };
        format!(
            "    - type: {kind}\n      auth: mediated\n{default}{optional}      # The model and effort new sessions start with, unless one is chosen.\n      # defaults:\n      #   model: <model>\n      #   effort: <effort>\n"
        )
    };
    harness("claudeCode", status.claude_code, claude_default) + &harness("codex", status.codex, !claude_default)
}

fn accounts(github: &Github) -> String {
    match github {
        Github::Owner(owner) => format!(
            "  # Act as the GitHub token `agentctl github login` stored for {owner}.\n  accounts:\n    - type: github\n      owner: {owner}\n"
        ),
        Github::Only => {
            "  # Act as the GitHub token `agentctl github login` stored.\n  accounts:\n    - type: github\n".to_owned()
        }
        Github::None => {
            "  # Run `agentctl github login` and uncomment to act as a GitHub account.\n  # accounts:\n  #   - type: github\n"
                .to_owned()
        }
    }
}

/// The repository's skills, listed from the agent directory, or a commented example.
fn skills_section(skills: &[String]) -> String {
    if skills.is_empty() {
        return "  # Skills every harness gets, each a directory with a SKILL.md, for example:\n  # skills:\n  #   - source: ../../.claude/skills/<skill>\n".to_owned();
    }
    let listed: String = skills
        .iter()
        .flat_map(|skill| ["    - source: ../../", skill.as_str(), "\n"])
        .collect();
    format!(
        "  # This repository's skills, for every harness and not only the one that finds them itself.\n  skills:\n{listed}"
    )
}

/// The variant of a cloning `agent.yaml` that works on the checkout instead.
pub(super) fn worktree_variant(choices: &Choices) -> String {
    format!(
        "# The agent in agent.yaml, working on this checkout instead of a clone: what it changes there
# changes on this machine. agentctl refuses to mount a checkout with a .env file in it, so give
# this variant its values from a file outside the checkout, chosen in the create form.
apiVersion: agents.platform/v1alpha1
kind: AgentVariant
extends: agent.yaml
metadata:
  name: {name}-worktree
spec:
  sandbox:
    mounts:
{bind}{tmp}",
        name = choices.name,
        bind = bind_mount(&choices.workspace),
        tmp = tmp_mount("Lists replace inherited ones, so this repeats agent.yaml's build scratch space.")
    )
}

fn bind_mount(workspace: &str) -> String {
    format!(
        "      - type: bind\n        source: ../..\n        target: {}/{workspace}\n        readOnly: false\n",
        agent::sandbox::platform::WORKING_DIRECTORY
    )
}

fn tmp_mount(comment: &str) -> String {
    format!("      # {comment}\n      - type: tmpfs\n        target: /tmp\n        capacity: \"{TMP_CAPACITY}\"\n")
}

/// The agent's own instructions, a starting point to replace.
pub(super) fn instructions(choices: &Choices) -> String {
    format!(
        "# {name}

You work on this repository in `{code}/{workspace}`. Its own CLAUDE.md or AGENTS.md describes the
project; this file is for what an agent should know beyond it, such as how to run the tests here
and what to check before opening a pull request.

`AGENT_NAME`, `AGENT_SESSION_NAME` and `AGENT_HOST` say which agent and session you are and on
which machine, so use them when you report where work happened.
",
        name = choices.name,
        code = agent::sandbox::platform::WORKING_DIRECTORY,
        workspace = choices.workspace
    )
}

/// Example values for `.env`, which stays on this machine.
pub(super) const ENV_SAMPLE: &str =
    "# Copy to .env and fill in what agent.yaml declares under environment and secrets. .env stays
# on this machine: plain values enter the agent as they are, and secrets only as placeholders.
# GitHub needs no entry here: `agentctl github login` stores its token in agentd.
#
# A checkout that the agent mounts must not contain .env, so for agent.worktree.yaml keep the
# file outside the checkout, for example ~/.agent/<name>.env, and choose it in the create form.

# GIT_USER_NAME=Your Name
# GIT_USER_EMAIL=you@example.com
# AZURE_DEVOPS_PAT=
";

/// Keeps local values and personal variants out of Git.
pub(super) const GITIGNORE: &str = ".env\nagent.mine.yaml\n";

/// Keeps the agent's local files out of its image's build context, which is this directory.
pub(super) const DOCKERIGNORE: &str =
    "# The image's build context is this directory; only what the Dockerfile copies belongs in it.
*
!Dockerfile
";

/// The agent's Dockerfile: the block generated from `tools`, the clone of `checkout` at boot, a
/// place for the person's own steps, and the base image's check of the result.
pub(super) fn dockerfile(tools: &crate::tools::Tools, checkout: &Checkout) -> String {
    let code = agent::sandbox::platform::WORKING_DIRECTORY;
    let repositories = match checkout {
        Checkout::Clone(repository) => format!(
            "# Repositories the agent clones into {code} at first boot, one OWNER/NAME [DESTINATION] per line.
RUN printf '%s\\n' '{repository}' >/etc/agent-workspace.d/workspace.list
"
        ),
        Checkout::Mount => format!(
            "# The agent works on the checkout mounted at {code}. To clone GitHub repositories at first boot
# instead, list them one OWNER/NAME [DESTINATION] per line:
# RUN printf '%s\\n' 'owner/name' >/etc/agent-workspace.d/workspace.list
"
        ),
    };
    format!(
        "# Created by `agentctl init`: the image this agent runs, built with Docker on the machine that
# runs agentctl. An agent keeps the image it was created with; `agentctl tools` offers to recreate
# it after a change.
#
# The block below is generated from tools.yaml: change the tools there, or with
# `agentctl tools add`. Steps of your own go after it, and agentctl leaves them as they are.
{block}
{repositories}
# Steps of your own, for example:
# RUN apt-get update \\
#     && apt-get install --yes --no-install-recommends postgresql-client \\
#     && rm -rf /var/lib/apt/lists/*

# Reports anything the steps above left that the agent platform cannot use.
RUN agent-image-check

USER agent
WORKDIR {code}
",
        block = tools.block()
    )
}

pub(crate) fn block(toolchain: &Toolchain) -> String {
    match toolchain {
        Toolchain::Node { version, corepack } => node(version.as_deref(), *corepack),
        Toolchain::Dotnet { sdk } => dotnet(sdk.as_deref()),
        Toolchain::Go { version } => go(version.as_deref()),
        Toolchain::Rust { channel } => rust(channel.as_deref()),
        Toolchain::Python => "# Python virtual environments and pip for the image's Python 3.
RUN apt-get update \\
    && apt-get install --yes --no-install-recommends python3-dev python3-pip python3-venv \\
    && rm -rf /var/lib/apt/lists/*
"
        .to_owned(),
        Toolchain::Java { maven } => format!(
            "# A JDK{}; Gradle projects bring their own wrapper.
RUN apt-get update \\
    && apt-get install --yes --no-install-recommends openjdk-21-jdk-headless{} \\
    && rm -rf /var/lib/apt/lists/*
",
            if *maven { " and Maven" } else { "" },
            if *maven { " maven" } else { "" }
        ),
    }
}

fn node(version: Option<&str>, corepack: bool) -> String {
    let install = version.map(|version| {
        // An exact version installs that release; a shorter one the latest of its major line.
        let release = if version.split('.').count() == 3 {
            format!("release/v{version}")
        } else {
            format!("release/latest-v{}.x", version.split('.').next().unwrap_or(version))
        };
        format!(
            "# Node.js {version}, in place of the base image's.
ARG NODE_RELEASE={release}
RUN case \"${{TARGETARCH}}\" in amd64) NODE_ARCH=x64 ;; arm64) NODE_ARCH=arm64 ;; esac \\
    && NODE_SUMS=\"$(curl -fsSL \"https://nodejs.org/download/${{NODE_RELEASE}}/SHASUMS256.txt\")\" \\
    && NODE_ARCHIVE=\"$(printf '%s\\n' \"${{NODE_SUMS}}\" | awk -v arch=\"linux-${{NODE_ARCH}}.tar.xz\" '$2 ~ (arch \"$\") {{ print $2; exit }}')\" \\
    && test -n \"${{NODE_ARCHIVE}}\" \\
    && curl -fsSL \"https://nodejs.org/download/${{NODE_RELEASE}}/${{NODE_ARCHIVE}}\" -o /tmp/node.tar.xz \\
    && printf '%s\\n' \"${{NODE_SUMS}}\" | awk -v name=\"${{NODE_ARCHIVE}}\" '$2 == name {{ print $1 \"  /tmp/node.tar.xz\" }}' | sha256sum -c - \\
    && tar -xJf /tmp/node.tar.xz --strip-components=1 -C /usr/local \\
    && rm /tmp/node.tar.xz \\
    && node --version
"
        )
    });
    let corepack = if corepack {
        "# The package manager package.json names, through Corepack.
RUN HOME=/root npm install --global corepack \\
    && rm -rf /root/.npm \\
    && corepack enable
"
    } else {
        ""
    };
    format!("{}{corepack}", install.unwrap_or_default())
}

fn dotnet(sdk: Option<&str>) -> String {
    let (what, selection) = sdk.map_or_else(
        || ("the latest LTS release".to_owned(), "--channel LTS".to_owned()),
        |sdk| (format!("{sdk}, as global.json asks"), format!("--version {sdk}")),
    );
    format!(
        "# The .NET SDK: {what}.
ENV DOTNET_ROOT=/usr/local/share/dotnet
ENV DOTNET_CLI_TELEMETRY_OPTOUT=1
ENV DOTNET_NOLOGO=1
ARG DOTNET_INSTALL_SCRIPT_COMMIT=6f559c420847ded38591392dafe785ad511f39f5
ARG DOTNET_INSTALL_SCRIPT_SHA256=082f7685e156738a1b2e2ed8381a621870d4ce8e8c59278034556f05c186eb2e
RUN apt-get update \\
    && apt-get install --yes --no-install-recommends libicu-dev \\
    && rm -rf /var/lib/apt/lists/* \\
    && case \"${{TARGETARCH}}\" in amd64) DOTNET_ARCH=x64 ;; arm64) DOTNET_ARCH=arm64 ;; esac \\
    && curl -fsSL \\
        \"https://raw.githubusercontent.com/dotnet/install-scripts/${{DOTNET_INSTALL_SCRIPT_COMMIT}}/src/dotnet-install.sh\" \\
        -o /tmp/dotnet-install.sh \\
    && echo \"${{DOTNET_INSTALL_SCRIPT_SHA256}}  /tmp/dotnet-install.sh\" | sha256sum -c - \\
    && bash /tmp/dotnet-install.sh --architecture \"${{DOTNET_ARCH}}\" --install-dir \"${{DOTNET_ROOT}}\" --no-path {selection} \\
    && ln -s \"${{DOTNET_ROOT}}/dotnet\" /usr/local/bin/dotnet \\
    && rm /tmp/dotnet-install.sh \\
    && HOME=/root dotnet --version
"
    )
}

fn go(version: Option<&str>) -> String {
    let what = version.map_or_else(
        || "the latest release".to_owned(),
        |version| format!("{version}, as go.mod asks"),
    );
    format!(
        "# Go: {what}. An empty GO_VERSION takes the latest release; a version without its patch
# number, the latest patch.
ARG GO_VERSION={version}
ENV PATH=/usr/local/go/bin:${{HOME}}/go/bin:${{PATH}}
RUN GO_FILE=\"$(curl -fsSL 'https://go.dev/dl/?mode=json&include=all' | jq -r --arg v \"${{GO_VERSION}}\" --arg arch \"${{TARGETARCH}}\" \\
        '[.[] | select(.stable and ($v == \"\" or .version == \"go\" + $v or (.version | startswith(\"go\" + $v + \".\"))))][0].files[] | select(.os == \"linux\" and .arch == $arch and .kind == \"archive\") | .filename + \" \" + .sha256')\" \\
    && test -n \"${{GO_FILE}}\" \\
    && curl -fsSL \"https://go.dev/dl/${{GO_FILE% *}}\" -o /tmp/go.tar.gz \\
    && echo \"${{GO_FILE#* }}  /tmp/go.tar.gz\" | sha256sum -c - \\
    && tar -xzf /tmp/go.tar.gz -C /usr/local \\
    && rm /tmp/go.tar.gz \\
    && HOME=/root go version
",
        version = version.unwrap_or_default()
    )
}

fn rust(channel: Option<&str>) -> String {
    let channel = channel.unwrap_or("stable");
    format!(
        "# Rust through rustup, at {channel}. The agent user owns RUSTUP_HOME, so a checkout that pins
# another toolchain installs it on first use. Cargo's home is the agent's own.
ARG RUST_TOOLCHAIN={channel}
ENV RUSTUP_HOME=/usr/local/rustup
ENV CARGO_HOME=${{HOME}}/.cargo
ENV PATH=${{HOME}}/.cargo/bin:${{PATH}}
RUN apt-get update \\
    && apt-get install --yes --no-install-recommends pkg-config libssl-dev rustup \\
    && rm -rf /var/lib/apt/lists/* \\
    && install -d -o agent -g agent /usr/local/rustup \"${{CARGO_HOME}}\" \\
    && rustup set profile minimal \\
    && rustup default \"${{RUST_TOOLCHAIN}}\" \\
    && rustup component add clippy rustfmt \\
    && chown -R agent:agent /usr/local/rustup \"${{CARGO_HOME}}\" \\
    && cargo --version
"
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::path::Path;

    use super::*;

    fn status(claude_code: bool, codex: bool) -> AuthenticationStatus {
        AuthenticationStatus {
            claude_code,
            codex,
            github: Vec::new(),
        }
    }

    fn choices(github: Github, tier: Tier, checkout: Checkout) -> Choices {
        Choices {
            name: "worker".into(),
            workspace: "app".into(),
            github,
            tier,
            checkout,
            size: Size {
                cpus: 2,
                memory_gib: 4,
                disk_gib: 16,
            },
            skills: Vec::new(),
        }
    }

    fn built() -> Choices {
        choices(Github::None, Tier::Developer, Checkout::Clone("digdir/app".into()))
    }

    fn decoded(text: &str) -> agent::Agent {
        agent::manifest::decode(text.as_bytes()).expect("the template is a valid manifest")
    }

    fn checkout_mount(mount: &agent::MountSpec) -> bool {
        let target = format!("{}/app", agent::sandbox::platform::WORKING_DIRECTORY);
        matches!(mount, agent::MountSpec::Bind { source, target: bound, read_only: false }
            if source == Path::new("../..") && bound.as_str() == target)
    }

    fn tmp_mount(mount: &agent::MountSpec) -> bool {
        matches!(mount, agent::MountSpec::Tmpfs { target, .. } if target.as_str() == "/tmp")
    }

    /// Uncomments each commented-out example block that starts with one of `keys`, as a person
    /// enabling it would, and fills in its placeholders.
    fn uncommented(text: &str, keys: &[&str]) -> String {
        let mut inside: Option<usize> = None;
        let mut lines = Vec::new();
        for line in text.lines() {
            let indent = line.len() - line.trim_start().len();
            let comment = line.trim_start().strip_prefix("# ");
            let starts = comment.is_some_and(|rest| keys.iter().any(|key| rest == format!("{key}:")));
            if starts {
                inside = Some(indent);
            } else if inside.is_some_and(|block| indent != block || comment.is_none_or(|rest| !rest.starts_with(' '))) {
                inside = None;
            }
            match (inside, comment) {
                (Some(_), Some(rest)) => lines.push(format!("{}{rest}", &line[..indent])),
                _ => lines.push(line.to_owned()),
            }
        }
        lines
            .join("\n")
            .replace("<model>", "sonnet")
            .replace("<effort>", "high")
            .replace("<skill>", "review")
    }

    #[test]
    fn signed_in_harnesses_are_required_and_the_others_optional() {
        let claude = decoded(&manifest(&built(), &status(true, false)));
        assert_eq!(claude.metadata.name, "worker");
        let kinds: Vec<_> = claude
            .spec
            .harnesses
            .iter()
            .map(|harness| (harness.kind, harness.optional))
            .collect();
        assert_eq!(
            kinds,
            [(agent::Harness::ClaudeCode, false), (agent::Harness::Codex, true)]
        );
        assert_eq!(
            claude.spec.default_harness().map(|harness| harness.kind),
            Some(agent::Harness::ClaudeCode)
        );
        assert!(claude.spec.ssh_access());

        let codex = decoded(&manifest(&built(), &status(false, true)));
        assert_eq!(
            codex.spec.default_harness().map(|harness| harness.kind),
            Some(agent::Harness::Codex)
        );
        assert!(
            codex
                .spec
                .harness(agent::Harness::ClaudeCode)
                .is_some_and(|claude| claude.optional)
        );

        let both = decoded(&manifest(&built(), &status(true, true)));
        assert!(both.spec.harnesses.iter().all(|harness| !harness.optional));
    }

    #[test]
    fn the_github_owner_is_named_only_when_a_token_applies() {
        let owned = decoded(&manifest(
            &choices(Github::Owner("digdir".into()), Tier::Developer, Checkout::Mount),
            &status(true, false),
        ));
        assert_eq!(owned.spec.github_owner(), Some("digdir"));
        let only = decoded(&manifest(
            &choices(Github::Only, Tier::Developer, Checkout::Mount),
            &status(true, false),
        ));
        assert!(only.spec.github_account());
        assert_eq!(only.spec.github_owner(), None);
        let without = manifest(&built(), &status(true, false));
        assert!(!decoded(&without).spec.github_account());
        assert!(without.contains("# accounts:"), "{without}");
    }

    #[test]
    fn the_size_home_instructions_and_scratch_space_are_in_the_manifest() {
        let agent = decoded(&manifest(&built(), &status(true, false)));
        let resources = serde_yaml_ng::to_value(agent.spec.sandbox.resources).expect("resources");
        assert_eq!(resources["cpu"], "2", "{resources:?}");
        assert_eq!(resources["memory"], "4Gi", "{resources:?}");
        assert_eq!(resources["rootFilesystem"]["capacity"], "16Gi", "{resources:?}");
        assert!(matches!(agent.spec.sandbox.mounts.as_slice(), [tmp] if tmp_mount(tmp)));
        assert_eq!(agent.spec.home.map(|home| home.source), Some("home".into()));
        assert_eq!(
            agent
                .spec
                .instructions
                .iter()
                .map(|instructions| instructions.source.clone())
                .collect::<Vec<_>>(),
            [std::path::PathBuf::from("instructions.md")]
        );
    }

    #[test]
    fn every_commented_example_is_valid_once_uncommented() {
        let text = manifest(&built(), &status(true, false));
        let keys = ["accounts", "defaults", "environment", "secrets", "network", "skills"];
        let enabled = uncommented(&text, &keys);
        let agent = agent::manifest::decode(enabled.as_bytes()).unwrap_or_else(|error| panic!("{error}\n{enabled}"));
        assert!(agent.spec.github_account(), "{enabled}");
        assert_eq!(agent.spec.environment.len(), 2, "{enabled}");
        assert_eq!(agent.spec.secrets.len(), 1, "{enabled}");
        assert_eq!(agent.spec.skills.len(), 1, "{enabled}");
        assert!(
            agent.spec.harnesses.iter().all(|harness| !harness.defaults.is_empty()),
            "{enabled}"
        );
        let network = serde_yaml_ng::to_value(&agent.spec.network).expect("network");
        assert_eq!(network["deny"][0], "metadata.google.internal", "{enabled}");
    }

    #[test]
    fn the_repositorys_skills_are_listed_from_the_agent_directory() {
        let mut with_skills = built();
        with_skills.skills = vec![".claude/skills/changelog".into(), ".claude/skills/review".into()];
        let agent = decoded(&manifest(&with_skills, &status(true, false)));
        assert_eq!(
            agent
                .spec
                .skills
                .iter()
                .map(|skill| skill.source.clone())
                .collect::<Vec<_>>(),
            [
                std::path::PathBuf::from("../../.claude/skills/changelog"),
                std::path::PathBuf::from("../../.claude/skills/review")
            ]
        );
    }

    #[test]
    fn a_cloning_agent_has_a_worktree_variant_that_mounts_the_checkout() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let manifest_path = directory.path().join("agent.yaml");
        let cloning = built();
        std::fs::write(&manifest_path, manifest(&cloning, &status(true, false))).expect("manifest");
        std::fs::write(directory.path().join(WORKTREE_VARIANT), worktree_variant(&cloning)).expect("variant");

        let base = agent::manifest::resolve(&manifest_path).expect("agent.yaml");
        assert!(matches!(base.agent.spec.sandbox.mounts.as_slice(), [tmp] if tmp_mount(tmp)));
        let variant = agent::manifest::resolve(&directory.path().join(WORKTREE_VARIANT)).expect("the variant");
        assert_eq!(variant.agent.metadata.name, "worker-worktree");
        assert!(
            matches!(variant.agent.spec.sandbox.mounts.as_slice(), [checkout, tmp] if checkout_mount(checkout) && tmp_mount(tmp)),
            "{:?}",
            variant.agent.spec.sandbox.mounts
        );
        assert_eq!(variant.agent.spec.sandbox.resources, base.agent.spec.sandbox.resources);
    }

    #[test]
    fn a_mounted_checkout_is_bound_in_the_manifest_itself() {
        let mounted = decoded(&manifest(
            &choices(Github::None, Tier::Developer, Checkout::Mount),
            &status(true, false),
        ));
        assert!(
            matches!(mounted.spec.sandbox.mounts.as_slice(), [checkout, tmp] if checkout_mount(checkout) && tmp_mount(tmp))
        );
    }

    #[test]
    fn only_a_desktop_opens_vnc() {
        let access = |tier: Tier| -> Vec<String> {
            let agent = decoded(&manifest(
                &choices(Github::None, tier, Checkout::Mount),
                &status(true, false),
            ));
            let value = serde_yaml_ng::to_value(&agent.spec.access).expect("access");
            value
                .as_sequence()
                .expect("a list")
                .iter()
                .map(|entry| entry["type"].as_str().unwrap_or_default().to_owned())
                .collect()
        };
        assert_eq!(access(Tier::Desktop), ["ssh", "vnc"]);
        for tier in [Tier::Basic, Tier::Developer, Tier::Web] {
            assert_eq!(access(tier), ["ssh"], "{tier:?}");
        }
    }

    #[test]
    fn a_built_image_comes_from_the_agent_directory() {
        let built = decoded(&manifest(&built(), &status(true, false)));
        let image = serde_yaml_ng::to_value(&built.spec.sandbox.image).expect("image");
        assert_eq!(image["type"], "build", "{image:?}");
        assert_eq!(image["context"], ".", "{image:?}");
        assert_eq!(image["dockerfile"], "Dockerfile", "{image:?}");
        assert!(
            DOCKERIGNORE.lines().any(|line| line == "*"),
            "nothing else enters the build context"
        );
        assert!(GITIGNORE.lines().any(|line| line == ".env"));
    }

    #[test]
    fn the_build_context_keeps_only_the_dockerfile() {
        // agentd reads .dockerignore with gitignore rules, matching each path or its parents.
        let directory = tempfile::TempDir::new().expect("temporary directory");
        std::fs::write(directory.path().join(".dockerignore"), DOCKERIGNORE).expect("dockerignore");
        let mut builder = ignore::gitignore::GitignoreBuilder::new(directory.path());
        assert!(builder.add(directory.path().join(".dockerignore")).is_none());
        let rules = builder.build().expect("rules");
        let excluded = |path: &str| rules.matched_path_or_any_parents(path, false).is_ignore();
        assert!(!excluded("Dockerfile"), "the Dockerfile is sent to the build");
        for left_out in [".env", "agent.yaml", "home/.bashrc", "instructions.md"] {
            assert!(excluded(left_out), "{left_out} stays out of the build");
        }
    }

    #[test]
    fn the_instructions_name_where_the_repository_is() {
        let text = instructions(&built());
        assert!(text.starts_with("# worker\n"), "{text}");
        assert!(
            text.contains(&format!("`{}/app`", agent::sandbox::platform::WORKING_DIRECTORY)),
            "{text}"
        );
    }

    #[test]
    fn the_dockerfile_clones_the_repository_and_installs_each_toolchain() {
        let detected = [
            Toolchain::Node {
                version: Some("20".into()),
                corepack: true,
            },
            Toolchain::Dotnet {
                sdk: Some("10.0.302".into()),
            },
            Toolchain::Go { version: None },
            Toolchain::Rust { channel: None },
            Toolchain::Python,
            Toolchain::Java { maven: true },
        ];
        let written = dockerfile(
            &crate::tools::Tools::from_detected(&detected, true, false, false),
            &Checkout::Clone("digdir/worker".into()),
        );
        for expected in [
            "\nFROM ghcr.io/digdir/agentctl-base:ubuntu-26.04@sha256:" as &str,
            "podman-docker",
            "\nRUN agent-image-check\n",
            "RUN printf '%s\\n' 'digdir/worker' >/etc/agent-workspace.d/workspace.list\n",
            "ARG NODE_RELEASE=release/latest-v20.x\n",
            "corepack enable",
            "--version 10.0.302",
            "ARG GO_VERSION=\n",
            "ARG RUST_TOOLCHAIN=stable\n",
            "python3-venv",
            "openjdk-21-jdk-headless maven",
        ] {
            assert!(written.contains(expected), "{expected} in {written}");
        }
        assert!(
            written
                .trim_end()
                .ends_with(&format!("WORKDIR {}", agent::sandbox::platform::WORKING_DIRECTORY)),
            "{written}"
        );

        let mounted = dockerfile(
            &crate::tools::Tools::from_detected(&[], false, false, false),
            &Checkout::Mount,
        );
        assert!(mounted.contains("# RUN printf"), "nothing is cloned: {mounted}");
        assert!(
            !mounted.contains("podman"),
            "no containers for a basic agent: {mounted}"
        );
        let desktop = dockerfile(
            &crate::tools::Tools::from_detected(&[], true, true, true),
            &Checkout::Mount,
        );
        assert!(desktop.contains("RUN agent-feature install desktop\n"), "{desktop}");
        assert!(!mounted.contains("NODE_RELEASE"), "{mounted}");
    }

    #[test]
    fn every_dockerfile_line_starts_an_instruction_or_continues_one() {
        let all = [
            Toolchain::Node {
                version: Some("20.11.1".into()),
                corepack: true,
            },
            Toolchain::Dotnet { sdk: None },
            Toolchain::Go {
                version: Some("1.26".into()),
            },
            Toolchain::Rust {
                channel: Some("1.97.1".into()),
            },
            Toolchain::Python,
            Toolchain::Java { maven: false },
        ];
        let written = dockerfile(
            &crate::tools::Tools::from_detected(&all, true, true, true),
            &Checkout::Clone("digdir/worker".into()),
        );
        let mut continued = false;
        for line in written.lines() {
            let trimmed = line.trim_start();
            if !continued && !trimmed.is_empty() && !trimmed.starts_with('#') {
                let instruction = trimmed.split_whitespace().next().unwrap_or_default();
                assert!(
                    ["FROM", "USER", "ARG", "ENV", "RUN", "WORKDIR", "COPY", "ENTRYPOINT"].contains(&instruction),
                    "{line:?} neither starts an instruction nor continues one in\n{written}"
                );
            }
            if !trimmed.starts_with('#') {
                continued = line.ends_with('\\');
            }
        }
        assert!(!continued, "the last instruction is complete");
    }

    #[test]
    fn an_exact_node_version_installs_that_release() {
        let block = node(Some("20.11.1"), false);
        assert!(block.contains("ARG NODE_RELEASE=release/v20.11.1\n"), "{block}");
        assert!(!block.contains("corepack"), "{block}");
    }
}
