#![allow(clippy::expect_used)]

use crate::support;

use std::path::{Path, PathBuf};

use agent::{AgentVariantName, definitions};

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
    std::fs::write(path, body).expect("manifest");
}

fn manifest(name: &str) -> String {
    serde_yaml_ng::to_string(&support::agent(name)).expect("manifest YAML")
}

fn variant(name: &str) -> String {
    format!(
        "apiVersion: agents.platform/v1alpha1\nkind: AgentVariant\nextends: agent.yaml\nmetadata:\n  name: {name}\n"
    )
}

/// An agent applied from `manifest_path`, as `agentd` reports it.
fn applied(name: &str, manifest_path: PathBuf, deleting: bool) -> agent::Agent {
    let mut agent = support::agent(name);
    agent.status.provenance = Some(agent::Provenance {
        source_directory: manifest_path.parent().expect("parent").to_path_buf(),
        manifest_path: Some(manifest_path),
        env_file: None,
    });
    if deleting {
        agent.metadata.deletion_timestamp = Some(time::OffsetDateTime::now_utc());
    }
    agent
}

/// Outside a repository, `agent.yaml` and `agents/<name>/` hold the definitions, and each is matched with the agents
/// applied from it.
#[test]
fn definitions_outside_a_repository_name_their_variant_and_the_agents_from_them() {
    let folder = support::TempDirectory::new("definitions");
    let home = folder.path();
    let notes = home.join("agents/notes/agent.yaml");
    let gpu = home.join("agents/notes/agent.gpu.yaml");
    let broken = home.join("agents/broken/agent.yaml");
    write(&notes, &manifest("notes"));
    write(&gpu, &variant("notes-gpu"));
    write(&broken, "kind: Agent\n");
    write(&home.join("agents/notes/README.md"), "not a manifest");
    write(&home.join("deeper/agents/hidden/agent.yaml"), &manifest("hidden"));

    let found = definitions::list(
        home,
        &[
            applied("notes", notes.clone(), false),
            applied("notes-2", notes.clone(), false),
            applied("gpu", gpu.clone(), false),
            applied("leaving", notes.clone(), true),
        ],
    );

    assert_eq!(
        found
            .iter()
            .map(|definition| definition.path.clone())
            .collect::<Vec<_>>(),
        [broken, notes, gpu],
        "a folder below `agents/` is not walked outside a repository"
    );
    let [broken, notes, gpu] = found.as_slice() else {
        panic!("three definitions");
    };
    assert_eq!(notes.name.as_deref(), Some("notes"));
    assert_eq!(notes.variant, None);
    assert_eq!(notes.agents, ["notes", "notes-2"], "a deleting agent is gone");
    assert_eq!(gpu.variant, Some(AgentVariantName::new("gpu").expect("variant")));
    assert_eq!(gpu.name.as_deref(), Some("notes-gpu"));
    assert_eq!(gpu.agents, ["gpu"]);
    assert!(broken.name.is_none() && broken.error.is_some(), "{broken:?}");
    assert!(notes.repository.is_none());

    let value = serde_json::to_value(notes).expect("JSON");
    assert_eq!(
        value.as_object().expect("object").keys().collect::<Vec<_>>(),
        ["agents", "name", "path"],
        "what a definition lacks stays out of it"
    );
}

/// Inside a repository, an agent is found from anywhere in it, and the definition names the repository.
#[test]
fn definitions_in_a_repository_are_found_from_any_folder_in_it() {
    let folder = support::TempDirectory::new("definitions-repository");
    let repository = folder.path();
    std::fs::create_dir_all(repository.join(".git")).expect("repository");
    let worker = repository.join("services/api/agents/worker/agent.yaml");
    write(&worker, &manifest("worker"));
    let nested = repository.join("docs/guide");
    std::fs::create_dir_all(&nested).expect("nested folder");

    let found = definitions::list(&nested, &[]);

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, worker);
    assert_eq!(found[0].repository.as_deref(), Some(repository));
    assert_eq!(found[0].agents, Vec::<String>::new());
}
