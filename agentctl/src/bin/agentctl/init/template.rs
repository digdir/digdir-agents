//! The Dockerfile blocks that install each toolchain an agent's image asks for.

use super::toolchains::Toolchain;

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
    use super::*;

    #[test]
    fn an_exact_node_version_installs_that_release() {
        let block = node(Some("20.11.1"), false);
        assert!(block.contains("ARG NODE_RELEASE=release/v20.11.1\n"), "{block}");
        assert!(!block.contains("corepack"), "{block}");
    }
}
