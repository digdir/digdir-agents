//! Embeds the agent definitions `agentctl setup` offers, the standard agents in `agents/standard/`
//! and the agent builder in `agents/agent-builder/`, so that setup works offline and always offers
//! agents its own manifest schema understands. A release archive holds only the two binaries,
//! which self-update requires, so the definitions travel inside `agentctl`. Their image sources
//! come along, so setup can build an image here that it cannot pull.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

fn main() {
    let manifest_directory = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let agents = manifest_directory.join("..").join("agents");
    let mut files = Vec::new();
    for folder in ["standard", "agent-builder"] {
        let folder = agents.join(folder);
        println!("cargo:rerun-if-changed={}", folder.display());
        if folder.is_dir() {
            collect(&agents, &folder, &mut files);
        }
    }
    files.sort();

    let mut source = String::from("&[\n");
    for (relative, absolute) in &files {
        let _ = writeln!(source, "    ({relative:?}, include_bytes!({absolute:?})),");
    }
    source.push(']');

    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default());
    if let Err(error) = std::fs::write(out.join("embedded_agents.rs"), source) {
        println!("cargo:warning=could not write the embedded agents: {error}");
        std::process::exit(1);
    }
}

fn collect(root: &Path, directory: &Path, files: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, files);
        } else if path.is_file() {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let absolute = std::fs::canonicalize(&path).unwrap_or(path);
            files.push((relative, absolute.to_string_lossy().into_owned()));
        }
    }
}
