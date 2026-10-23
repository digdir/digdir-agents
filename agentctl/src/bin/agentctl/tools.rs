//! `tools.yaml`: the tools an agent folder's image installs, which agentctl turns into the
//! generated block of the folder's Dockerfile, and `agentctl tools`, which changes them.
//!
//! The file belongs to the folder, so every manifest in it (`agent.yaml`, `agent.worktree.yaml`,
//! a local `agent.mine.yaml`) builds the same image. An agent keeps the image it was created
//! with, so agentctl records which Dockerfile each agent was created from and offers to recreate
//! an agent whose tools have changed since.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use agent::{Error, control_api::Client, local::home::ControlPlaneHome};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::{
    CommandResult,
    init::{image, template, toolchains::Toolchain},
    prompt,
};

/// The file beside an agent's manifests that lists its tools.
pub(crate) const TOOLS_FILE: &str = "tools.yaml";
/// The Dockerfile the generated block is written into.
pub(crate) const DOCKERFILE: &str = "Dockerfile";
/// The line that starts the generated block.
const BEGIN: &str = "# --- Generated from tools.yaml by agentctl. Change tools.yaml, not this block. ---";
/// The line that ends the generated block.
const END: &str = "# --- End of the generated block ---";
/// Where agentctl records the Dockerfile each agent was created from, in its home.
const STAMPS_FILE: &str = "agent-images.json";

/// The tools an agent folder's image installs.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Tools {
    /// Podman, with `docker` and `docker compose` working through it.
    #[serde(default)]
    pub(crate) containers: bool,
    /// Chromium with playwright-cli, the base image's browser feature.
    #[serde(default)]
    pub(crate) browser: bool,
    /// A desktop with VNC access, the base image's desktop feature, which brings the browser.
    #[serde(default)]
    pub(crate) desktop: bool,
    #[serde(default)]
    pub(crate) toolchains: Toolchains,
    /// Ubuntu packages.
    #[serde(default)]
    pub(crate) packages: Vec<String>,
    /// Global npm packages.
    #[serde(default)]
    pub(crate) npm: Vec<String>,
}

/// Language toolchains, by the version each one installs.
#[expect(clippy::struct_excessive_bools, reason = "mirrors tools.yaml's yes-or-no settings")]
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Toolchains {
    /// A Node.js version other than the base image's, such as `20` or `20.11.1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) node: Option<String>,
    /// Corepack, for the package manager `package.json` names.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) corepack: bool,
    /// The .NET SDK: `lts`, or an SDK version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dotnet: Option<String>,
    /// Go: `latest`, or a version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) go: Option<String>,
    /// Rust: `stable`, or a toolchain channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) rust: Option<String>,
    /// Python virtual environments and pip.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) python: bool,
    /// A JDK.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) java: bool,
    /// Maven, with the JDK.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) maven: bool,
}

impl Tools {
    /// The tools a new agent starts with: what the repository's files ask for, and what the
    /// person chose the agent to do.
    pub(crate) fn from_detected(detected: &[Toolchain], containers: bool, browser: bool, desktop: bool) -> Self {
        let mut toolchains = Toolchains::default();
        for toolchain in detected {
            match toolchain {
                Toolchain::Node { version, corepack } => {
                    toolchains.node.clone_from(version);
                    toolchains.corepack = *corepack;
                }
                Toolchain::Dotnet { sdk } => {
                    toolchains.dotnet = Some(sdk.clone().unwrap_or_else(|| "lts".to_owned()));
                }
                Toolchain::Go { version } => {
                    toolchains.go = Some(version.clone().unwrap_or_else(|| "latest".to_owned()));
                }
                Toolchain::Rust { channel } => {
                    toolchains.rust = Some(channel.clone().unwrap_or_else(|| "stable".to_owned()));
                }
                Toolchain::Python => toolchains.python = true,
                Toolchain::Java { maven } => {
                    toolchains.java = true;
                    toolchains.maven = *maven;
                }
            }
        }
        Self {
            containers,
            browser,
            desktop,
            toolchains,
            packages: Vec::new(),
            npm: Vec::new(),
        }
    }

    /// The toolchains as the Dockerfile blocks know them, in a fixed order.
    fn toolchains(&self) -> Vec<Toolchain> {
        let chains = &self.toolchains;
        let named = |value: &Option<String>, unpinned: &str| {
            value.clone().filter(|version| !version.eq_ignore_ascii_case(unpinned))
        };
        let mut toolchains = Vec::new();
        if chains.node.is_some() || chains.corepack {
            toolchains.push(Toolchain::Node {
                version: chains.node.clone(),
                corepack: chains.corepack,
            });
        }
        if chains.dotnet.is_some() {
            toolchains.push(Toolchain::Dotnet {
                sdk: named(&chains.dotnet, "lts"),
            });
        }
        if chains.go.is_some() {
            toolchains.push(Toolchain::Go {
                version: named(&chains.go, "latest"),
            });
        }
        if let Some(channel) = &chains.rust {
            toolchains.push(Toolchain::Rust {
                channel: Some(channel.clone()),
            });
        }
        if chains.python {
            toolchains.push(Toolchain::Python);
        }
        if chains.java || chains.maven {
            toolchains.push(Toolchain::Java { maven: chains.maven });
        }
        toolchains
    }

    /// Rejects anything that would not be one plain word in a Dockerfile `RUN` line.
    fn validate(&self) -> Result<(), Error> {
        let plain = |word: &str, extra: &[char]| {
            !word.is_empty()
                && word.chars().all(|character| {
                    character.is_ascii_alphanumeric() || ".-_+".contains(character) || extra.contains(&character)
                })
        };
        for (name, version) in [
            ("node", &self.toolchains.node),
            ("dotnet", &self.toolchains.dotnet),
            ("go", &self.toolchains.go),
            ("rust", &self.toolchains.rust),
        ] {
            if let Some(version) = version
                && !plain(version, &[])
            {
                return Err(Error::Invalid(format!(
                    "tools.yaml: {name}: {version:?} is not a version"
                )));
            }
        }
        if let Some(package) = self.packages.iter().find(|package| !plain(package, &[])) {
            return Err(Error::Invalid(format!(
                "tools.yaml: packages: {package:?} is not a package name"
            )));
        }
        if let Some(package) = self.npm.iter().find(|package| !plain(package, &['@', '/', '~'])) {
            return Err(Error::Invalid(format!(
                "tools.yaml: npm: {package:?} is not a package name"
            )));
        }
        Ok(())
    }

    /// The tools in a person's words, one per line.
    pub(crate) fn describe(&self) -> Vec<String> {
        let yes = |value: bool| if value { "yes" } else { "no" };
        let toolchains: Vec<String> = self.toolchains().iter().map(Toolchain::describe).collect();
        let list = |items: &[String]| {
            if items.is_empty() {
                "none".to_owned()
            } else {
                items.join(", ")
            }
        };
        vec![
            format!("Containers  {}", yes(self.containers)),
            format!("Browser     {}", yes(self.browser || self.desktop)),
            format!("Desktop     {}", yes(self.desktop)),
            format!("Toolchains  {}", list(&toolchains)),
            format!("Packages    {}", list(&self.packages)),
            format!("npm         {}", list(&self.npm)),
        ]
    }

    /// `tools.yaml` as agentctl writes it, explaining each setting.
    pub(crate) fn render(&self) -> String {
        let chains = &self.toolchains;
        let mut toolchains = String::new();
        for (key, value) in [
            ("node", chains.node.clone()),
            ("corepack", chains.corepack.then(|| "true".to_owned())),
            ("dotnet", chains.dotnet.clone()),
            ("go", chains.go.clone()),
            ("rust", chains.rust.clone()),
            ("python", chains.python.then(|| "true".to_owned())),
            ("java", chains.java.then(|| "true".to_owned())),
            ("maven", chains.maven.then(|| "true".to_owned())),
        ] {
            if let Some(value) = value {
                let quoted = if value == "true" { value } else { format!("\"{value}\"") };
                toolchains.push_str("  ");
                toolchains.push_str(key);
                toolchains.push_str(": ");
                toolchains.push_str(&quoted);
                toolchains.push('\n');
            }
        }
        let toolchains = if toolchains.is_empty() {
            "toolchains: {}\n".to_owned()
        } else {
            format!("toolchains:\n{toolchains}")
        };
        let list = |items: &[String]| {
            if items.is_empty() {
                " []\n".to_owned()
            } else {
                // An entry such as `@angular/cli` cannot start a plain YAML value.
                items
                    .iter()
                    .map(|item| {
                        if item.starts_with('@') {
                            format!("\n  - \"{item}\"")
                        } else {
                            format!("\n  - {item}")
                        }
                    })
                    .collect::<String>()
                    + "\n"
            }
        };
        format!(
            "# The tools this agent's image installs. agentctl writes them into the Dockerfile's generated
# block. Change them with `agentctl tools add` and `agentctl tools remove`, or here; then run
# `agentctl tools`, which offers to recreate the agent with them.

# Podman, with `docker` and `docker compose`.
containers: {containers}
# Chromium with playwright-cli, for testing web apps.
browser: {browser}
# A desktop you can watch and take over over VNC; it brings the browser, and agent.yaml's access
# needs `- type: vnc` to open it.
desktop: {desktop}
# Language toolchains: node (a version), corepack, dotnet (\"lts\" or an SDK version), go (\"latest\"
# or a version), rust (\"stable\" or a channel), python, java and maven.
{toolchains}# Ubuntu packages, such as postgresql-client.
packages:{packages}# Global npm packages, such as pnpm.
npm:{npm}",
            containers = self.containers,
            browser = self.browser,
            desktop = self.desktop,
            packages = list(&self.packages),
            npm = list(&self.npm),
        )
    }

    /// The Dockerfile's generated block, markers included.
    pub(crate) fn block(&self) -> String {
        let containers = if self.containers {
            format!("\n{}", image::CONTAINERS)
        } else {
            String::new()
        };
        let toolchains: String = self
            .toolchains()
            .iter()
            .flat_map(|toolchain| ["\n".to_owned(), template::block(toolchain)])
            .collect();
        let packages = if self.packages.is_empty() {
            String::new()
        } else {
            format!(
                "\n# Ubuntu packages.\nRUN apt-get update \\\n    && apt-get install --yes --no-install-recommends {} \\\n    && rm -rf /var/lib/apt/lists/*\n",
                self.packages.join(" ")
            )
        };
        let npm = if self.npm.is_empty() {
            String::new()
        } else {
            format!(
                "\n# Global npm packages.\nRUN HOME=/root npm install --global {} \\\n    && rm -rf /root/.npm\n",
                self.npm.join(" ")
            )
        };
        // The base image's own features go after the repository's tools, as its README advises.
        let features = if self.desktop {
            format!("\n{}", image::DESKTOP)
        } else if self.browser {
            format!("\n{}", image::BROWSER)
        } else {
            String::new()
        };
        format!(
            "{BEGIN}\nFROM {base}\n\nUSER root\nARG TARGETARCH\n{containers}{toolchains}{packages}{npm}{features}{END}\n",
            base = image::BASE_IMAGE
        )
    }

    /// Adds `tool`, as `agentctl tools add` names it, returning what it added.
    fn add(&mut self, tool: &str) -> Result<String, Error> {
        let (name, version) = tool
            .split_once('@')
            .map_or((tool, None), |(name, version)| (name, Some(version.to_owned())));
        let chains = &mut self.toolchains;
        let added = match name {
            "containers" | "docker" | "podman" => {
                self.containers = true;
                "containers".to_owned()
            }
            "browser" | "chromium" | "playwright" => {
                self.browser = true;
                "a browser".to_owned()
            }
            "desktop" | "vnc" => {
                self.desktop = true;
                "a desktop".to_owned()
            }
            "node" | "nodejs" => {
                chains.node = version;
                "Node.js".to_owned()
            }
            "corepack" => {
                chains.corepack = true;
                "Corepack".to_owned()
            }
            "dotnet" | ".net" => {
                chains.dotnet = Some(version.unwrap_or_else(|| "lts".to_owned()));
                ".NET".to_owned()
            }
            "go" | "golang" => {
                chains.go = Some(version.unwrap_or_else(|| "latest".to_owned()));
                "Go".to_owned()
            }
            "rust" | "cargo" => {
                chains.rust = Some(version.unwrap_or_else(|| "stable".to_owned()));
                "Rust".to_owned()
            }
            "python" | "python3" => {
                chains.python = true;
                "Python".to_owned()
            }
            "java" | "jdk" => {
                chains.java = true;
                "Java".to_owned()
            }
            "maven" | "mvn" => {
                chains.java = true;
                chains.maven = true;
                "Maven".to_owned()
            }
            _ => {
                if let Some(package) = tool.strip_prefix("npm:") {
                    if !self.npm.iter().any(|existing| existing == package) {
                        self.npm.push(package.to_owned());
                    }
                    format!("{package} from npm")
                } else {
                    if !self.packages.iter().any(|existing| existing == tool) {
                        self.packages.push(tool.to_owned());
                    }
                    format!("the Ubuntu package {tool}")
                }
            }
        };
        self.validate()?;
        Ok(added)
    }

    /// Removes `tool`, as `agentctl tools remove` names it, returning what it removed, or
    /// `None` when the tools did not have it.
    fn remove(&mut self, tool: &str) -> Option<String> {
        let name = tool.split_once('@').map_or(tool, |(name, _)| name);
        let chains = &mut self.toolchains;
        let taken = |flag: &mut bool| std::mem::replace(flag, false);
        match name {
            "containers" | "docker" | "podman" => taken(&mut self.containers).then(|| "containers".to_owned()),
            "browser" | "chromium" | "playwright" => taken(&mut self.browser).then(|| "the browser".to_owned()),
            "desktop" | "vnc" => taken(&mut self.desktop).then(|| "the desktop".to_owned()),
            "node" | "nodejs" => chains.node.take().map(|_| "the Node.js version".to_owned()),
            "corepack" => taken(&mut chains.corepack).then(|| "Corepack".to_owned()),
            "dotnet" | ".net" => chains.dotnet.take().map(|_| ".NET".to_owned()),
            "go" | "golang" => chains.go.take().map(|_| "Go".to_owned()),
            "rust" | "cargo" => chains.rust.take().map(|_| "Rust".to_owned()),
            "python" | "python3" => taken(&mut chains.python).then(|| "Python".to_owned()),
            "java" | "jdk" => {
                chains.maven = false;
                taken(&mut chains.java).then(|| "Java".to_owned())
            }
            "maven" | "mvn" => taken(&mut chains.maven).then(|| "Maven".to_owned()),
            _ => {
                let (list, package) = match tool.strip_prefix("npm:") {
                    Some(package) => (&mut self.npm, package),
                    None => (&mut self.packages, tool),
                };
                let before = list.len();
                list.retain(|existing| existing != package);
                (list.len() != before).then(|| package.to_owned())
            }
        }
    }
}

/// Reads `directory`'s `tools.yaml`, or `None` when it has none.
///
/// # Errors
///
/// Returns an error when the file cannot be read or is not valid.
pub(crate) fn read(directory: &Path) -> Result<Option<Tools>, Error> {
    let path = directory.join(TOOLS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let tools: Tools =
        serde_yaml_ng::from_str(&text).map_err(|error| Error::Invalid(format!("{}: {error}", path.display())))?;
    tools.validate()?;
    Ok(Some(tools))
}

/// `dockerfile` with its generated block replaced by `tools`'s, or `None` when it has no block.
fn with_block(dockerfile: &str, tools: &Tools) -> Option<String> {
    let start = dockerfile.find(BEGIN)?;
    let end = dockerfile[start..].find(END)? + start;
    let after = dockerfile[end..]
        .find('\n')
        .map_or(dockerfile.len(), |offset| end + offset + 1);
    Some(format!(
        "{}{}{}",
        &dockerfile[..start],
        tools.block(),
        &dockerfile[after..]
    ))
}

/// Writes `directory`'s Dockerfile block from its `tools.yaml`, when it has both. Returns whether
/// the Dockerfile changed.
///
/// # Errors
///
/// Returns an error when the files cannot be read or written, `tools.yaml` is not valid, or the
/// Dockerfile has no generated block to write into.
pub(crate) fn sync(directory: &Path) -> Result<bool, Error> {
    let Some(tools) = read(directory)? else {
        return Ok(false);
    };
    let path = directory.join(DOCKERFILE);
    let current = match std::fs::read_to_string(&path) {
        Ok(current) => current,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::Invalid(format!(
                "{} has a {TOOLS_FILE} but no {DOCKERFILE}: agentctl writes the tools into the Dockerfile beside the manifest",
                directory.display()
            )));
        }
        Err(error) => return Err(error.into()),
    };
    let Some(updated) = with_block(&current, &tools) else {
        return Err(Error::Invalid(format!(
            "{} has no block generated from tools.yaml; put `{BEGIN}` and `{END}` lines where it goes",
            path.display()
        )));
    };
    if updated == current {
        return Ok(false);
    }
    std::fs::write(&path, updated)?;
    Ok(true)
}

/// The Dockerfile `directory`'s agents would be built from now, after syncing `tools.yaml`.
fn projected(directory: &Path) -> Option<String> {
    let current = std::fs::read_to_string(directory.join(DOCKERFILE)).ok()?;
    match read(directory).ok().flatten() {
        Some(tools) => with_block(&current, &tools).or(Some(current)),
        None => Some(current),
    }
}

fn digest(text: &str) -> String {
    use std::fmt::Write as _;

    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

fn stamps_path(home: &ControlPlaneHome) -> PathBuf {
    home.path().join(STAMPS_FILE)
}

fn stamps(home: &ControlPlaneHome) -> BTreeMap<String, String> {
    std::fs::read_to_string(stamps_path(home))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Records that `agent` was just created from the Dockerfile in `directory`.
pub(crate) fn record(home: &ControlPlaneHome, agent: &str, directory: &Path) {
    let Some(dockerfile) = std::fs::read_to_string(directory.join(DOCKERFILE)).ok() else {
        return;
    };
    let mut stamps = stamps(home);
    stamps.insert(agent.to_owned(), digest(&dockerfile));
    if let Ok(text) = serde_json::to_string_pretty(&stamps) {
        let _ = std::fs::write(stamps_path(home), text);
    }
}

/// Whether `agent`, created from `directory`, runs an older image than its Dockerfile and
/// `tools.yaml` now describe; `None` when agentctl did not record its creation.
pub(crate) fn outdated(home: &ControlPlaneHome, agent: &str, directory: &Path) -> Option<bool> {
    let recorded = stamps(home).remove(agent)?;
    Some(digest(&projected(directory)?) != recorded)
}

/// The agents among `agents` whose tools changed since they were created.
pub(crate) fn outdated_agents(home: &ControlPlaneHome, agents: &[agent::Agent]) -> std::collections::HashSet<String> {
    agents
        .iter()
        .filter(|agent| {
            agent
                .status
                .provenance
                .as_ref()
                .and_then(|provenance| provenance.manifest_path.as_deref())
                .and_then(Path::parent)
                .and_then(|folder| outdated(home, &agent.metadata.name, folder))
                == Some(true)
        })
        .map(|agent| agent.metadata.name.clone())
        .collect()
}

/// The folder whose tools `agentctl tools` changes: the current directory's agent's, with that
/// agent and the manifest it was created from, or else the current directory when it has a
/// `tools.yaml`.
async fn target(client: &Client, agent: Option<&str>) -> CommandResult<(PathBuf, Option<(String, PathBuf)>)> {
    if let Some((folder, agent)) = agent_folder(client, agent).await? {
        return Ok((folder, Some(agent)));
    }
    let directory = std::env::current_dir().map_err(Error::from)?;
    if directory.join(TOOLS_FILE).is_file() {
        return Ok((directory, None));
    }
    if directory.join(agent::manifest::MANIFEST_FILE).is_file() {
        return Err(kept_by_hand(&directory).into());
    }
    Err(Error::Invalid("no agent here; `agentctl init` sets one up, or pass --agent NAME".into()).into())
}

/// The folder of `agent`, or of the current directory's agent, with its name and the manifest it
/// was created from; `None` when no agent matches the current directory.
pub(crate) async fn agent_folder(
    client: &Client,
    agent: Option<&str>,
) -> CommandResult<Option<(PathBuf, (String, PathBuf))>> {
    let directory = std::env::current_dir().map_err(Error::from)?;
    let found = match agent {
        Some(name) => Some(client.get(name).await?),
        None => match client.resolve_agent_variant(directory.clone(), None).await {
            Ok(agent) => Some(agent),
            Err(Error::Rpc(error)) if error.is_not_found() => None,
            Err(Error::Rpc(error)) if error.is_invalid_params() => {
                return Err(Error::Invalid(
                    "several agents match this directory; run this in one agent's folder, or pass --agent".into(),
                )
                .into());
            }
            Err(error) => return Err(error.into()),
        },
    };
    if let Some(agent) = found
        && let Some(manifest) = agent
            .status
            .provenance
            .as_ref()
            .and_then(|provenance| provenance.manifest_path.clone())
        && let Some(folder) = manifest.parent().map(Path::to_path_buf)
    {
        return Ok(Some((folder, (agent.metadata.name, manifest))));
    }
    Ok(None)
}

/// The answer for an agent folder without `tools.yaml`, whose image is not agentctl's to write.
fn kept_by_hand(folder: &Path) -> Error {
    Error::Invalid(format!(
        "{} has no {TOOLS_FILE}: its image is kept by hand in its Dockerfile, so there are no tools for agentctl to change",
        crate::format::home_relative(folder)
    ))
}

/// `agentctl tools`: shows the tools, adds or removes some, and offers to recreate the agent
/// when its image is older than its tools.
///
/// # Errors
///
/// Returns an error when the folder has no `tools.yaml`, a tool is not valid, or recreating fails.
pub(crate) async fn run(
    home: &ControlPlaneHome,
    client: &Client,
    agent: Option<&str>,
    add: &[String],
    remove: &[String],
) -> CommandResult<()> {
    let (folder, agent) = target(client, agent).await?;
    let mut tools = read(&folder)?.ok_or_else(|| kept_by_hand(&folder))?;
    let mut changes = Vec::new();
    for tool in add {
        changes.push(format!("Added {}.", tools.add(tool)?));
    }
    for tool in remove {
        match tools.remove(tool) {
            Some(removed) => changes.push(format!("Removed {removed}.")),
            None => changes.push(format!("{tool} was not among the tools.")),
        }
    }
    if !add.is_empty() || !remove.is_empty() {
        std::fs::write(folder.join(TOOLS_FILE), tools.render()).map_err(Error::from)?;
        sync(&folder)?;
        for change in &changes {
            println!("{change}");
        }
    }
    println!();
    for line in tools.describe() {
        println!("  {line}");
    }
    let Some((name, manifest)) = agent else {
        return Ok(());
    };
    if outdated(home, &name, &folder) != Some(true) {
        return Ok(());
    }
    if !prompt::interactive() {
        println!("\n{name} runs the image it was created with; recreate it to use these tools.");
        return Ok(());
    }
    if !prompt::confirm_lines(&[
        &format!("{name} runs the image it was created with, without these changes."),
        "Recreate it now with them? Its disk starts over.",
    ])? {
        return Ok(());
    }
    recreate(home, client, &name, &manifest).await
}

/// Deletes `name` and creates it again from `manifest`, waiting until it is Ready.
async fn recreate(home: &ControlPlaneHome, client: &Client, name: &str, manifest: &Path) -> CommandResult<()> {
    client.delete(name).await?;
    eprintln!("Deleting {name}…");
    loop {
        match client.get(name).await {
            Err(Error::Rpc(error)) if error.is_not_found() => break,
            Err(error) => return Err(error.into()),
            Ok(_) => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
        }
    }
    crate::tui::create_agent(home, client, manifest.to_path_buf(), name.to_owned(), None).await?;
    crate::wait_until_converged_simply(client, name, std::time::Duration::from_mins(15)).await?;
    eprintln!("{name} is ready with its new tools.");
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn sample() -> Tools {
        Tools {
            containers: true,
            toolchains: Toolchains {
                corepack: true,
                go: Some("1.26".into()),
                ..Toolchains::default()
            },
            ..Tools::default()
        }
    }

    #[test]
    fn the_written_file_reads_back_as_the_same_tools() {
        let mut tools = sample();
        tools.packages = vec!["jq".into(), "postgresql-client".into()];
        tools.npm = vec!["@angular/cli".into()];
        let read: Tools = serde_yaml_ng::from_str(&tools.render()).expect("tools.yaml");
        assert_eq!(read, tools);
        let empty: Tools = serde_yaml_ng::from_str(&Tools::default().render()).expect("tools.yaml");
        assert_eq!(empty, Tools::default());
    }

    #[test]
    fn the_block_installs_each_tool_on_the_right_base_image() {
        let mut tools = sample();
        tools.packages = vec!["jq".into()];
        tools.npm = vec!["pnpm".into()];
        let block = tools.block();
        assert!(block.starts_with(BEGIN) && block.trim_end().ends_with(END), "{block}");
        for expected in [
            "FROM ghcr.io/digdir/agentctl-base:ubuntu-26.04@sha256:",
            "podman-docker",
            "corepack enable",
            "ARG GO_VERSION=1.26",
            "apt-get install --yes --no-install-recommends jq",
            "npm install --global pnpm",
        ] {
            assert!(block.contains(expected), "{expected} in {block}");
        }
        assert!(!block.contains("agent-feature"), "no browser asked for");
        tools.browser = true;
        let browser = tools.block();
        assert!(
            browser.contains("RUN agent-feature install browser\nENV NODE_PATH="),
            "{browser}"
        );
        tools.desktop = true;
        let desktop = tools.block();
        assert!(desktop.contains("RUN agent-feature install desktop\n"), "{desktop}");
        assert!(
            desktop.contains("ENV DISPLAY=:1") && !desktop.contains("install browser"),
            "{desktop}"
        );
    }

    #[test]
    fn syncing_rewrites_only_the_generated_block() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let tools = sample();
        std::fs::write(
            directory.path().join(TOOLS_FILE),
            serde_yaml_ng::to_string(&tools).expect("YAML"),
        )
        .expect("tools.yaml");
        let dockerfile = format!("# header\n{BEGIN}\nFROM old\n{END}\n\n# mine\nRUN echo kept\n");
        std::fs::write(directory.path().join(DOCKERFILE), &dockerfile).expect("Dockerfile");

        assert!(sync(directory.path()).expect("synced"));
        let synced = std::fs::read_to_string(directory.path().join(DOCKERFILE)).expect("Dockerfile");
        assert!(
            synced.starts_with("# header\n") && synced.ends_with("\n# mine\nRUN echo kept\n"),
            "{synced}"
        );
        assert!(!synced.contains("FROM old"), "{synced}");
        assert!(
            !sync(directory.path()).expect("synced again"),
            "nothing changes the second time"
        );

        std::fs::write(directory.path().join(DOCKERFILE), "FROM mine\n").expect("Dockerfile");
        sync(directory.path()).expect_err("no block to write into");
    }

    #[test]
    fn a_folder_without_tools_is_left_alone_and_tools_without_a_dockerfile_say_where_it_goes() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        assert!(!sync(directory.path()).expect("nothing to sync"), "a hand-kept folder");
        std::fs::write(
            directory.path().join(TOOLS_FILE),
            serde_yaml_ng::to_string(&sample()).expect("YAML"),
        )
        .expect("tools.yaml");
        let error = sync(directory.path()).expect_err("no Dockerfile beside the manifest");
        assert!(error.to_string().contains("no Dockerfile"), "{error}");
    }

    #[test]
    fn tools_are_added_and_removed_by_the_names_people_use() {
        let mut tools = Tools::default();
        assert_eq!(tools.add("go@1.25").expect("go"), "Go");
        assert_eq!(tools.toolchains.go.as_deref(), Some("1.25"));
        assert_eq!(tools.add("dotnet").expect("dotnet"), ".NET");
        assert_eq!(tools.toolchains.dotnet.as_deref(), Some("lts"));
        assert_eq!(tools.add("jq").expect("apt"), "the Ubuntu package jq");
        assert_eq!(tools.add("npm:pnpm").expect("npm"), "pnpm from npm");
        assert_eq!(tools.add("docker").expect("containers"), "containers");
        tools.add("jq").expect("again");
        assert_eq!(tools.packages, ["jq"], "added once");
        tools.add("rm -rf /").expect_err("not a package name");

        let mut tools = Tools::default();
        tools.add("maven").expect("maven");
        assert!(tools.toolchains.java && tools.toolchains.maven);
        assert_eq!(tools.remove("java").as_deref(), Some("Java"));
        assert!(!tools.toolchains.maven, "Maven goes with its JDK");
        assert_eq!(tools.remove("go"), None);
    }

    #[test]
    fn an_agent_is_outdated_once_its_tools_change() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let state = tempfile::TempDir::new().expect("state");
        let home = ControlPlaneHome::resolve(Some(state.path())).expect("home");
        let tools = sample();
        std::fs::write(
            directory.path().join(TOOLS_FILE),
            serde_yaml_ng::to_string(&tools).expect("YAML"),
        )
        .expect("tools.yaml");
        std::fs::write(
            directory.path().join(DOCKERFILE),
            format!("{}\nRUN mine\n", tools.block()),
        )
        .expect("Dockerfile");

        assert_eq!(outdated(&home, "worker", directory.path()), None, "not recorded");
        record(&home, "worker", directory.path());
        assert_eq!(outdated(&home, "worker", directory.path()), Some(false));

        let mut changed = tools;
        changed.packages.push("jq".into());
        std::fs::write(directory.path().join(TOOLS_FILE), changed.render()).expect("tools.yaml");
        assert_eq!(
            outdated(&home, "worker", directory.path()),
            Some(true),
            "before syncing too"
        );
    }
}
