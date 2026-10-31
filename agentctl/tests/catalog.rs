#![allow(clippy::expect_used, clippy::panic)]

//! The agents `agentctl setup` offers, the standard agents in `agents/standard/` and the agent
//! builder in `agents/agent-builder/`: each must resolve to a valid agent on a published image,
//! built by a bake target of its own.

use std::path::Path;

use agent::catalog::Catalog;
use sandbox::image::ImageSource;

fn catalog_directory() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/standard")
}

#[test]
fn every_standard_agent_resolves_on_a_published_image_with_a_bake_target() {
    let directory = catalog_directory();
    if !directory.join(agent::catalog::CATALOG_FILE).is_file() {
        return;
    }
    let catalog = Catalog::from_directory(directory).expect("the catalog should parse");
    let bake = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../docker-bake.hcl"))
        .expect("docker-bake.hcl should be readable");
    for archetype in catalog.archetypes() {
        let staging = tempfile::tempdir().expect("a staging directory");
        let manifest = catalog
            .extract(&archetype.id, staging.path())
            .expect("the archetype should extract");
        let agent = agent::manifest::resolve(&manifest)
            .unwrap_or_else(|error| panic!("{}: {error}", archetype.id))
            .agent;
        let ImageSource::Reference { reference } = &agent.spec.sandbox.image else {
            panic!(
                "{}: a standard agent must be on a published image, not built",
                archetype.id
            );
        };
        if archetype.hidden {
            continue;
        }
        assert!(
            bake.contains(&format!("\"standard-{}\"", archetype.id)),
            "{}: docker-bake.hcl has no target standard-{0}",
            archetype.id
        );
        assert!(
            reference.contains(&format!("agent-{}", archetype.id)),
            "{}: its image {reference} is not the one its bake target publishes",
            archetype.id
        );
    }
}

#[test]
fn the_embedded_catalog_matches_the_directory() {
    let directory = catalog_directory();
    let embedded = Catalog::embedded().expect("the embedded catalog should parse");
    let ids = |catalog: &Catalog| {
        catalog
            .archetypes()
            .iter()
            .map(|archetype| archetype.id.clone())
            .collect::<Vec<_>>()
    };
    // Setup builds an image here from the Dockerfile it carries when the published one cannot
    // be pulled.
    for archetype in embedded.visible() {
        let staging = tempfile::tempdir().expect("a staging directory");
        embedded
            .extract(&archetype.id, staging.path())
            .expect("the archetype should extract");
        assert!(
            staging.path().join(&archetype.id).join("Dockerfile").is_file(),
            "{}: agentctl does not carry the Dockerfile of its image",
            archetype.id
        );
    }
    if directory.join(agent::catalog::CATALOG_FILE).is_file() {
        let on_disk = Catalog::from_directory(directory).expect("the catalog should parse");
        assert_eq!(ids(&embedded), ids(&on_disk));
    } else {
        assert_eq!(ids(&embedded), Vec::<String>::new());
    }
}

#[test]
fn agentctl_carries_the_agent_builder_on_its_published_image_with_a_bake_target() {
    let staging = tempfile::tempdir().expect("a staging directory");
    let manifest = agent::builder::extract(staging.path()).expect("the agent builder should extract");
    let agent = agent::manifest::resolve(&manifest)
        .expect("the agent builder should resolve")
        .agent;
    let ImageSource::Reference { reference } = &agent.spec.sandbox.image else {
        panic!("the agent builder must be on a published image, not built");
    };
    let bake = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../docker-bake.hcl"))
        .expect("docker-bake.hcl should be readable");
    assert!(
        bake.contains("target \"agent-builder\""),
        "docker-bake.hcl has no target agent-builder"
    );
    assert!(
        reference.contains("agent-agent-builder"),
        "its image {reference} is not the one its bake target publishes"
    );
    // Setup builds the image here from the Dockerfile it carries when the published one cannot be
    // pulled.
    assert!(
        manifest.with_file_name("Dockerfile").is_file(),
        "agentctl does not carry the Dockerfile of the agent builder's image"
    );
}
