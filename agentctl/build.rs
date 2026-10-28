//! Embeds the standard agents in `agents/standard/` so that `agentctl setup` works offline and
//! always offers the archetypes its own manifest schema understands. A release archive holds only
//! the two binaries, which self-update requires, so the catalog travels inside `agentctl`. Their
//! image sources come along, so setup can build an image here that it cannot pull.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

fn main() {
    let manifest_directory = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let catalog = manifest_directory.join("..").join("agents").join("standard");
    println!("cargo:rerun-if-changed={}", catalog.display());

    let mut files = Vec::new();
    if catalog.is_dir() {
        collect(&catalog, &catalog, &mut files);
    }
    files.sort();

    let mut source = String::from("&[\n");
    for (relative, absolute) in &files {
        let _ = writeln!(source, "    ({relative:?}, include_bytes!({absolute:?})),");
    }
    source.push(']');

    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default());
    if let Err(error) = std::fs::write(out.join("catalog_files.rs"), source) {
        println!("cargo:warning=could not write the embedded catalog: {error}");
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
