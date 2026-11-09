//! Embeds each folder of `bundled/definitions/` as a bundled agent definition, with every file in it, for
//! `agent::bundled` to list. Cargo runs this again whenever anything in that folder changes.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "a build script reports a definition it cannot embed by failing the build"
)]

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("bundled/definitions");
    println!("cargo::rerun-if-changed={}", root.display());
    let mut generated = String::from("&[\n");
    for definition in sorted_entries(&root).into_iter().filter(|path| path.is_dir()) {
        let name = file_name(&definition);
        let mut files = Vec::new();
        collect_files(&definition, &definition, &mut files);
        writeln!(
            generated,
            "    EmbeddedDefinition {{\n        name: {name:?},\n        files: &["
        )
        .expect("writing to a String succeeds");
        for (relative, path, executable) in files {
            writeln!(
                generated,
                "            EmbeddedFile {{ path: {relative:?}, contents: include_bytes!({:?}), executable: {executable} }},",
                path.display().to_string(),
            )
            .expect("writing to a String succeeds");
        }
        generated.push_str("        ],\n    },\n");
    }
    generated.push(']');
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR")).join("bundled_definitions.rs");
    std::fs::write(output, generated).expect("the bundled definitions should be writable to OUT_DIR");
}

/// The entries of `directory`, by name, or none when it does not exist.
fn sorted_entries(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut paths = entries
        .map(|entry| entry.expect("a bundled definition entry should be readable").path())
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

/// Adds every file below `directory` as its `/`-separated path relative to `definition`, its absolute path and
/// whether it is executable.
fn collect_files(definition: &Path, directory: &Path, files: &mut Vec<(String, PathBuf, bool)>) {
    for path in sorted_entries(directory) {
        let metadata = std::fs::symlink_metadata(&path).expect("a bundled definition file should be readable");
        assert!(
            !metadata.file_type().is_symlink(),
            "{} is a symbolic link; a bundled definition holds only files and folders",
            path.display()
        );
        if metadata.is_dir() {
            collect_files(definition, &path, files);
        } else {
            let relative = path
                .strip_prefix(definition)
                .expect("files are below their definition")
                .components()
                .map(|component| component.as_os_str().to_str().expect("bundled file names are UTF-8"))
                .collect::<Vec<_>>()
                .join("/");
            files.push((relative, path.clone(), executable(&metadata)));
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .expect("bundled definition names are UTF-8")
        .to_owned()
}

#[cfg(unix)]
fn executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_metadata: &std::fs::Metadata) -> bool {
    false
}
