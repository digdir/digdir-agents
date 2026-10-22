//! `tools.yaml`: the tools an agent folder's image installs, which agentctl turns into the
//! generated block of the folder's Dockerfile.
//!
//! The file belongs to the folder, so every manifest in it (`agent.yaml`, `agent.worktree.yaml`,
//! a local `agent.mine.yaml`) builds the same image.

use std::path::Path;

use agent::Error;
use serde::{Deserialize, Serialize};

use crate::init::{image, template, toolchains::Toolchain};

/// The file beside an agent's manifests that lists its tools.
pub(crate) const TOOLS_FILE: &str = "tools.yaml";
/// The Dockerfile the generated block is written into.
pub(crate) const DOCKERFILE: &str = "Dockerfile";
/// The line that starts the generated block.
const BEGIN: &str = "# --- Generated from tools.yaml by agentctl. Change tools.yaml, not this block. ---";
/// The line that ends the generated block.
const END: &str = "# --- End of the generated block ---";

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
}
