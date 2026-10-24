//! The integration tests, as modules of one test binary: a binary per file would link every dependency once per
//! file.

mod architecture;
mod backend;
mod docker_context;
mod network_runtime;
mod runtime;

/// Cargo builds no test file that is not a module here, since the crate turns its automatic test targets off.
#[test]
#[allow(clippy::expect_used)]
fn every_test_file_is_a_module() {
    let tests = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let modules = std::fs::read_to_string(tests.join("integration.rs")).expect("integration.rs should be readable");
    for entry in std::fs::read_dir(&tests).expect("the tests directory should be readable") {
        let path = entry.expect("a tests directory entry should be readable").path();
        let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.extension().is_some_and(|extension| extension == "rs") && name != "integration" {
            assert!(
                modules.contains(&format!("\nmod {name};\n")),
                "tests/{name}.rs should be a module of integration.rs"
            );
        }
    }
}
