//! The language toolchains an agent's image can install, at the versions a repository asks for.

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
