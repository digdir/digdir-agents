//! Which toolchains a repository needs, read from the files that declare them.
//!
//! A marker file anywhere in the repository's tracked files, up to [`MAX_DEPTH`] directories
//! deep, selects a toolchain; versions come from the shallowest file that names one. Without Git,
//! the directory's files that its ignore files do not exclude count instead.

use std::path::{Path, PathBuf};

/// How many directories below the root a marker file may be, so monorepos with their projects
/// under `src/` or `apps/` are covered without reading the whole tree.
const MAX_DEPTH: usize = 3;
/// The Node.js major version the base image carries.
pub(super) const BASE_NODE_MAJOR: u32 = 22;

/// A toolchain the agent's image installs, with the version the repository asks for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Toolchain {
    /// Node.js beyond the base image's: another version, or Corepack for `packageManager`.
    Node {
        /// An exact version (`20.11.1`) or a major version (`20`).
        version: Option<String>,
        corepack: bool,
    },
    /// The .NET SDK, at `global.json`'s version or the latest LTS.
    Dotnet { sdk: Option<String> },
    /// Go, at `go.mod`'s toolchain or go version, or the latest release.
    Go { version: Option<String> },
    /// Rust through rustup, at the toolchain file's channel or stable.
    Rust { channel: Option<String> },
    /// Python virtual environments and pip on the image's Python 3.
    Python,
    /// A JDK, with Maven for a `pom.xml`; Gradle projects bring their own wrapper.
    Java { maven: bool },
}

impl Toolchain {
    /// A short name for listing what was found.
    pub(crate) fn describe(&self) -> String {
        let versioned = |name: &str, version: &Option<String>| {
            version
                .as_ref()
                .map_or_else(|| name.to_owned(), |version| format!("{name} {version}"))
        };
        match self {
            Self::Node { version, corepack } => {
                let node = versioned("Node.js", version);
                if *corepack {
                    format!("{node} with Corepack")
                } else {
                    node
                }
            }
            Self::Dotnet { sdk } => versioned(".NET SDK", sdk),
            Self::Go { version } => versioned("Go", version),
            Self::Rust { channel } => versioned("Rust", channel),
            Self::Python => "Python".to_owned(),
            Self::Java { maven: true } => "Java with Maven".to_owned(),
            Self::Java { maven: false } => "Java".to_owned(),
        }
    }
}

/// The toolchains `root` needs beyond the base image, in a fixed order.
pub(super) fn detect(root: &Path) -> Vec<Toolchain> {
    let files = candidate_files(root);
    let named = |names: &[&str]| -> Vec<PathBuf> {
        files
            .iter()
            .filter(|file| {
                file.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| names.contains(&name))
            })
            .map(|file| root.join(file))
            .collect()
    };
    let with_extension = |extensions: &[&str]| {
        files.iter().any(|file| {
            file.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extensions.contains(&extension))
        })
    };
    let read = |paths: &[PathBuf], parse: fn(&str) -> Option<String>| {
        paths
            .iter()
            .find_map(|path| std::fs::read_to_string(path).ok().as_deref().and_then(parse))
    };

    let mut toolchains = Vec::new();
    let package_files = named(&["package.json"]);
    if !package_files.is_empty() {
        let version =
            read(&named(&[".nvmrc", ".node-version"]), node_version_file).or_else(|| read(&package_files, node_engine));
        let corepack = read(&package_files, package_manager).is_some();
        let version = version.filter(|version| node_major(version) != Some(BASE_NODE_MAJOR) || version.contains('.'));
        if version.is_some() || corepack {
            toolchains.push(Toolchain::Node { version, corepack });
        }
    }
    let global_json = named(&["global.json"]);
    if !global_json.is_empty() || with_extension(&["csproj", "fsproj", "sln", "slnx"]) {
        toolchains.push(Toolchain::Dotnet {
            sdk: read(&global_json, dotnet_sdk),
        });
    }
    let go_mod = named(&["go.mod"]);
    if !go_mod.is_empty() {
        toolchains.push(Toolchain::Go {
            version: read(&go_mod, go_version),
        });
    }
    let cargo = named(&["Cargo.toml"]);
    if !cargo.is_empty() {
        let channel = read(&named(&["rust-toolchain.toml"]), rust_toolchain_toml)
            .or_else(|| read(&named(&["rust-toolchain"]), rust_toolchain_file));
        toolchains.push(Toolchain::Rust { channel });
    }
    if !named(&["pyproject.toml", "requirements.txt", "setup.py", "Pipfile", "uv.lock"]).is_empty() {
        toolchains.push(Toolchain::Python);
    }
    let maven = !named(&["pom.xml"]).is_empty();
    if maven
        || !named(&[
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
            "settings.gradle.kts",
        ])
        .is_empty()
    {
        toolchains.push(Toolchain::Java { maven });
    }
    toolchains
}

/// The repository's tracked and untracked but not ignored files up to [`MAX_DEPTH`], shallowest
/// first, relative to `root`.
fn candidate_files(root: &Path) -> Vec<PathBuf> {
    let mut files = git_files(root).unwrap_or_else(|| walked_files(root));
    files.sort_by_key(|path| path.components().count());
    files
}

fn git_files(root: &Path) -> Option<Vec<PathBuf>> {
    if !crate::host::git_available() {
        return None;
    }
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    Some(
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| PathBuf::from(String::from_utf8_lossy(path).into_owned()))
            .filter(|path| path.components().count() <= MAX_DEPTH + 1)
            .collect(),
    )
}

/// The files Git would list, read without Git: `.gitignore` and other ignore files apply.
fn walked_files(root: &Path) -> Vec<PathBuf> {
    ignore::WalkBuilder::new(root)
        .max_depth(Some(MAX_DEPTH + 1))
        .require_git(false)
        .hidden(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .follow_links(false)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.path().strip_prefix(root).ok().map(Path::to_path_buf))
        .collect()
}

/// `.nvmrc` and `.node-version`: a version such as `20`, `v20.11.1` or `20.11`; aliases such
/// as `lts/iron` are not versions and are ignored.
fn node_version_file(contents: &str) -> Option<String> {
    let version = contents.lines().next()?.trim().trim_start_matches('v');
    version
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| version.to_owned())
}

/// `engines.node` when it pins a major version (`20`, `20.x`, `^20.1.0`, `~20`); ranges such as
/// `>=18` are met by the base image and are ignored.
fn node_engine(contents: &str) -> Option<String> {
    let package: serde_json::Value = serde_json::from_str(contents).ok()?;
    let engine = package.get("engines")?.get("node")?.as_str()?.trim();
    let pinned = engine.trim_start_matches(['^', '~']);
    let major: String = pinned.chars().take_while(char::is_ascii_digit).collect();
    let rest = &pinned[major.len()..];
    (!major.is_empty() && (rest.is_empty() || rest.starts_with('.')) && !engine.contains(' ')).then_some(major)
}

fn package_manager(contents: &str) -> Option<String> {
    let package: serde_json::Value = serde_json::from_str(contents).ok()?;
    package.get("packageManager")?.as_str().map(str::to_owned)
}

fn node_major(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

fn dotnet_sdk(contents: &str) -> Option<String> {
    let global: serde_json::Value = serde_json::from_str(contents).ok()?;
    global.get("sdk")?.get("version")?.as_str().map(str::to_owned)
}

/// `go.mod`'s `toolchain go1.22.3`, else its `go 1.22.0` line.
fn go_version(contents: &str) -> Option<String> {
    let directive = |name: &str| {
        contents.lines().find_map(|line| {
            let mut words = line.split_whitespace();
            (words.next() == Some(name)).then(|| words.next()).flatten()
        })
    };
    directive("toolchain")
        .and_then(|toolchain| toolchain.strip_prefix("go"))
        .or_else(|| directive("go"))
        .map(str::to_owned)
}

/// `channel = "1.97.1"` under `[toolchain]` in `rust-toolchain.toml`.
fn rust_toolchain_toml(contents: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let value = line.trim().strip_prefix("channel")?.trim_start().strip_prefix('=')?;
        Some(value.trim().trim_matches(['"', '\'']).to_owned()).filter(|channel| !channel.is_empty())
    })
}

/// The legacy `rust-toolchain` file, which holds only the channel.
fn rust_toolchain_file(contents: &str) -> Option<String> {
    let channel = contents.lines().next()?.trim();
    (!channel.is_empty() && !channel.starts_with('[')).then(|| channel.to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn repository(files: &[(&str, &str)]) -> tempfile::TempDir {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        for (path, contents) in files {
            let path = directory.path().join(path);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("directories");
            std::fs::write(path, contents).expect("file");
        }
        let initialized = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(directory.path())
            .status()
            .expect("git");
        assert!(initialized.success());
        directory
    }

    #[test]
    fn a_repository_without_markers_needs_nothing_beyond_the_base() {
        let directory = repository(&[("README.md", "# hello\n")]);
        assert!(detect(directory.path()).is_empty());
    }

    #[test]
    fn every_toolchain_is_found_with_its_declared_version() {
        let directory = repository(&[
            ("package.json", r#"{"packageManager": "yarn@4.18.0"}"#),
            (".nvmrc", "v20.11.1\n"),
            (
                "global.json",
                r#"{"sdk": {"version": "10.0.302", "rollForward": "latestFeature"}}"#,
            ),
            ("src/Api/Api.csproj", "<Project/>"),
            (
                "tools/go.mod",
                "module example.com/tools\n\ngo 1.24.0\n\ntoolchain go1.26.4\n",
            ),
            ("Cargo.toml", "[workspace]\n"),
            (
                "rust-toolchain.toml",
                "[toolchain]\nchannel = \"1.97.1\"\ncomponents = [\"clippy\"]\n",
            ),
            ("scripts/requirements.txt", "requests\n"),
            ("java/pom.xml", "<project/>"),
        ]);
        assert_eq!(
            detect(directory.path()),
            [
                Toolchain::Node {
                    version: Some("20.11.1".into()),
                    corepack: true
                },
                Toolchain::Dotnet {
                    sdk: Some("10.0.302".into())
                },
                Toolchain::Go {
                    version: Some("1.26.4".into())
                },
                Toolchain::Rust {
                    channel: Some("1.97.1".into())
                },
                Toolchain::Python,
                Toolchain::Java { maven: true },
            ]
        );
    }

    #[test]
    fn the_base_images_node_is_enough_unless_another_version_or_corepack_is_asked_for() {
        let base = repository(&[("package.json", r#"{"engines": {"node": ">=18"}}"#)]);
        assert!(detect(base.path()).is_empty());
        let same_major = repository(&[("package.json", "{}"), (".nvmrc", "22\n")]);
        assert!(detect(same_major.path()).is_empty());
        let older = repository(&[("package.json", r#"{"engines": {"node": "^20.1.0"}}"#)]);
        assert_eq!(
            detect(older.path()),
            [Toolchain::Node {
                version: Some("20".into()),
                corepack: false
            }]
        );
        let alias = repository(&[("package.json", "{}"), (".nvmrc", "lts/iron\n")]);
        assert!(detect(alias.path()).is_empty());
    }

    #[test]
    fn versions_are_optional_and_markers_too_deep_are_ignored() {
        let directory = repository(&[
            ("App.sln", ""),
            ("go.mod", "module example.com/app\n"),
            ("rust-toolchain", "stable\n"),
            ("Cargo.toml", ""),
            ("build.gradle.kts", ""),
            ("a/b/c/d/pyproject.toml", ""),
        ]);
        assert_eq!(
            detect(directory.path()),
            [
                Toolchain::Dotnet { sdk: None },
                Toolchain::Go { version: None },
                Toolchain::Rust {
                    channel: Some("stable".into())
                },
                Toolchain::Java { maven: false },
            ]
        );
    }

    #[test]
    fn a_git_checkout_skips_ignored_files() {
        let directory = repository(&[(".gitignore", "vendor/\n"), ("vendor/go.mod", "module x\n")]);
        assert!(detect(directory.path()).is_empty());
    }

    #[test]
    fn outside_git_subdirectories_are_read_and_ignore_files_apply() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        for (path, contents) in [
            (".gitignore", "vendor/\n"),
            (".git/HEAD", ""),
            ("go.mod", "module x\n"),
            ("sub/Cargo.toml", ""),
            ("vendor/pom.xml", ""),
        ] {
            let path = directory.path().join(path);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("directories");
            std::fs::write(path, contents).expect("file");
        }
        let mut files = walked_files(directory.path());
        files.sort();
        assert_eq!(
            files,
            [
                PathBuf::from(".gitignore"),
                "go.mod".into(),
                PathBuf::from("sub").join("Cargo.toml")
            ]
        );
    }

    #[test]
    fn found_toolchains_read_as_a_list() {
        assert_eq!(
            Toolchain::Node {
                version: Some("20".into()),
                corepack: true
            }
            .describe(),
            "Node.js 20 with Corepack"
        );
        assert_eq!(Toolchain::Dotnet { sdk: None }.describe(), ".NET SDK");
        assert_eq!(Toolchain::Java { maven: true }.describe(), "Java with Maven");
    }
}
