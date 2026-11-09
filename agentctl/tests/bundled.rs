//! Agents agentd runs from the definitions it carries: `ensure`, their lifetimes, their workspaces,
//! and agentd's copies of their definitions.

#![allow(clippy::expect_used)]

use std::{collections::BTreeMap, path::Path, rc::Rc, time::Duration};

use agent::{
    Agent, Error, Lifetime, MountSpec, Origin, RunState,
    bundled::{Definition, DefinitionFile, Definitions, Snapshots, Workspace},
    control_plane::{AgentStore, ControlPlane, EnsureOutcome, EnsureRequest, Ensured, Reconciler, memory},
    progress::ProvisioningState,
};
use sandbox::{image::ImageSource, memory as sandbox_memory};

use crate::{
    control_plane::{MemoryProvider, sandbox_service},
    support::{IgnoreNotifications, TempDirectory, agent},
};

const BASE_IMAGE: &str = "ghcr.io/digdir/agentctl-base:ubuntu-26.04";

/// The files of a definition named `name`, whose agent reads `instructions` and a home holding
/// `home`, on `image`.
fn files(name: &str, image: &str, instructions: &str, home: &str) -> BTreeMap<String, DefinitionFile> {
    let mut manifest = agent(name);
    manifest.spec.sandbox.image = ImageSource::Reference {
        reference: image.into(),
    };
    let file = |contents: &[u8]| DefinitionFile {
        contents: contents.to_vec().into(),
        executable: false,
    };
    BTreeMap::from([
        (
            "agent.yaml".to_owned(),
            file(serde_yaml_ng::to_string(&manifest).expect("manifest").as_bytes()),
        ),
        ("instructions.md".to_owned(), file(instructions.as_bytes())),
        ("home/.profile".to_owned(), file(home.as_bytes())),
    ])
}

fn builder(instructions: &str) -> Definition {
    builder_with(BASE_IMAGE, instructions, "home")
}

fn builder_with(image: &str, instructions: &str, home: &str) -> Definition {
    Definition::new(
        "builder",
        Lifetime::UntilDeleted,
        Some(Workspace::ReadOnly {
            writable: vec!["agents".into()],
        }),
        files("builder", image, instructions, home),
    )
    .expect("builder definition")
}

fn sign_in() -> Definition {
    Definition::new(
        "sign-in",
        Lifetime::Ephemeral,
        None,
        files("sign-in", BASE_IMAGE, "sign in", "home"),
    )
    .expect("sign-in definition")
}

pub(crate) struct Fixture {
    pub(crate) store: Rc<memory::InMemoryAgentStore>,
    home: TempDirectory,
    workspaces: TempDirectory,
    pub(crate) control_plane: Rc<ControlPlane>,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let store = Rc::new(memory::InMemoryAgentStore::new());
        let home = TempDirectory::new("bundled-home");
        let control_plane = Rc::new(control_plane(&store, home.path(), [builder("one"), sign_in()]));
        Self {
            store,
            home,
            workspaces: TempDirectory::new("bundled-workspaces"),
            control_plane,
        }
    }

    /// A control plane over the same agents and home as another build of agentd, which carries
    /// `definitions`.
    fn rebuilt(&self, definitions: impl IntoIterator<Item = Definition>) -> ControlPlane {
        control_plane(&self.store, self.home.path(), definitions)
    }

    fn snapshots(&self) -> Snapshots {
        Snapshots::new(self.home.path().join("bundled"))
    }

    pub(crate) fn workspace(&self, name: &str) -> std::path::PathBuf {
        let path = self.workspaces.path().join(name);
        std::fs::create_dir_all(&path).expect("workspace");
        path
    }

    fn snapshot_folders(&self) -> Vec<String> {
        let mut names = std::fs::read_dir(self.home.path().join("bundled"))
            .map(|entries| {
                entries
                    .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

fn control_plane(
    store: &Rc<memory::InMemoryAgentStore>,
    home: &Path,
    definitions: impl IntoIterator<Item = Definition>,
) -> ControlPlane {
    ControlPlane::new(store.clone(), Rc::new(IgnoreNotifications)).with_bundled(
        Definitions::new(definitions),
        Snapshots::new(home.join("bundled")),
        home.to_path_buf(),
    )
}

pub(crate) fn request(definition: &str, workspace: Option<&Path>) -> EnsureRequest {
    EnsureRequest {
        definition: definition.into(),
        workspace: workspace.map(Path::to_path_buf),
        lifetime: None,
    }
}

async fn ensure(control_plane: &ControlPlane, definition: &str, workspace: Option<&Path>) -> Ensured {
    control_plane
        .ensure(request(definition, workspace))
        .await
        .expect("ensure")
        .0
}

fn digest(agent: &Agent) -> String {
    match agent.status.origin.as_ref() {
        Some(Origin::Bundled { digest, .. }) => Some(digest.clone()),
        _ => None,
    }
    .expect("a bundled agent")
}

#[tokio::test(flavor = "local")]
async fn ensure_makes_the_agent_with_its_workspace_and_its_definitions_copy() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("Some Repo");

    let ensured = ensure(&fixture.control_plane, "builder", Some(&workspace)).await;

    assert_eq!(ensured.outcome, EnsureOutcome::Created);
    let agent = ensured.agent;
    assert_eq!(agent.metadata.name, "builder-some-repo");
    assert_eq!(agent.spec.run_state(), RunState::Running);
    assert!(
        matches!(&agent.status.origin, Some(Origin::Bundled { definition, .. }) if definition == "builder"),
        "{:?}",
        agent.status.origin
    );
    assert_eq!(
        agent.status.provenance, None,
        "agentd's copy is not a definition of the person's"
    );
    assert_eq!(agent.status.lifetime, None, "kept until deleted");
    let workspace = workspace.canonicalize().expect("canonical workspace");
    assert_eq!(agent.status.workspace.as_deref(), Some(workspace.as_path()));
    assert_eq!(
        agent.spec.sandbox.mounts,
        [
            MountSpec::Bind {
                source: workspace.clone(),
                target: sandbox::SandboxPath::new("/home/agent/code/Some Repo"),
                read_only: true,
                optional: false,
            },
            MountSpec::Bind {
                source: workspace.join("agents"),
                target: sandbox::SandboxPath::new("/home/agent/code/Some Repo/agents"),
                read_only: false,
                optional: false,
            },
        ]
    );
    assert!(
        workspace.join("agents").is_dir(),
        "the folder the agent may change is made"
    );

    let record = fixture.store.get_by_name("builder-some-repo").await.expect("stored");
    assert_eq!(record.source_directory, fixture.snapshots().path(&digest(&agent)));
    assert_eq!(
        std::fs::read_to_string(record.source_directory.join("instructions.md")).expect("copied instructions"),
        "one"
    );
}

#[tokio::test(flavor = "local")]
async fn concurrent_ensures_make_one_agent_for_a_workspace() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    let other = fixture.workspace("other");

    let (first, second, elsewhere) = tokio::join!(
        ensure(&fixture.control_plane, "builder", Some(&workspace)),
        ensure(&fixture.control_plane, "builder", Some(&workspace)),
        ensure(&fixture.control_plane, "builder", Some(&other)),
    );

    let mut outcomes = [first.outcome, second.outcome];
    outcomes.sort_by_key(|outcome| format!("{outcome:?}"));
    assert_eq!(outcomes, [EnsureOutcome::Created, EnsureOutcome::Resumed]);
    assert_eq!(first.agent.metadata.name, second.agent.metadata.name);
    assert_eq!(elsewhere.outcome, EnsureOutcome::Created);
    assert_eq!(elsewhere.agent.metadata.name, "builder-other");
    let names = fixture
        .store
        .list()
        .await
        .expect("agents")
        .into_iter()
        .map(|record| record.agent.metadata.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["builder-other", "builder-repo"]);
}

#[tokio::test(flavor = "local")]
async fn asking_again_resumes_the_agent_and_starts_it() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    let created = ensure(&fixture.control_plane, "builder", Some(&workspace)).await;
    fixture
        .control_plane
        .set_run_state("builder-repo", RunState::Stopped)
        .await
        .expect("stop");

    let resumed = ensure(&fixture.control_plane, "builder", Some(&workspace)).await;

    assert_eq!(resumed.outcome, EnsureOutcome::Resumed);
    assert_eq!(resumed.agent.metadata.name, created.agent.metadata.name);
    assert_eq!(resumed.agent.spec.run_state(), RunState::Running);
    let again = ensure(&fixture.control_plane, "builder", Some(&workspace)).await;
    assert_eq!(again.outcome, EnsureOutcome::Resumed);
    assert_eq!(
        again.agent.metadata.generation, resumed.agent.metadata.generation,
        "a running agent of the same definition is left as it is"
    );
}

#[tokio::test(flavor = "local")]
async fn a_newer_definition_updates_the_agent_in_place_and_its_old_copy_goes() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    let created = ensure(&fixture.control_plane, "builder", Some(&workspace)).await;

    let rebuilt = fixture.rebuilt([builder("two")]);
    let updated = ensure(&rebuilt, "builder", Some(&workspace)).await;

    assert_eq!(updated.outcome, EnsureOutcome::Updated);
    assert_eq!(updated.agent.metadata.name, created.agent.metadata.name);
    assert_ne!(digest(&updated.agent), digest(&created.agent));
    let record = fixture.store.get_by_name("builder-repo").await.expect("stored");
    assert_eq!(
        std::fs::read_to_string(record.source_directory.join("instructions.md")).expect("new instructions"),
        "two"
    );
    assert_eq!(
        fixture.snapshot_folders(),
        [digest(&updated.agent)],
        "no agent uses the old copy any more"
    );
}

#[tokio::test(flavor = "local")]
async fn a_definition_that_changes_what_cannot_change_in_place_reports_the_agent_outdated() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    let created = ensure(&fixture.control_plane, "builder", Some(&workspace)).await;

    for (definition, field) in [
        (
            builder_with("ghcr.io/digdir/agentctl-base:ubuntu-26.04-desktop", "one", "home"),
            "spec.sandbox.image",
        ),
        (builder_with(BASE_IMAGE, "one", "a new home"), "spec.home"),
    ] {
        let rebuilt = fixture.rebuilt([definition]);
        let outdated = rebuilt
            .ensure(request("builder", Some(&workspace)))
            .await
            .expect("an outdated agent is an outcome, not an error")
            .0;

        let EnsureOutcome::Outdated { reason } = outdated.outcome else {
            panic!("{field}: {:?}", outdated.outcome);
        };
        assert!(reason.contains(field), "{reason}");
        assert_eq!(outdated.agent.metadata.generation, created.agent.metadata.generation);
        assert_eq!(
            digest(&outdated.agent),
            digest(&created.agent),
            "the agent is left as it was"
        );
    }
    let record = fixture.store.get_by_name("builder-repo").await.expect("stored");
    assert_eq!(
        record.source_directory,
        fixture.snapshots().path(&digest(&created.agent))
    );
    assert!(record.source_directory.is_dir(), "the copy the agent uses stays");
}

#[tokio::test(flavor = "local")]
async fn apply_and_ensure_never_change_an_agent_of_the_other_origin() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    let source = TempDirectory::new("bundled-local");
    let apply = |name: &str| agent::control_plane::ApplyRequest {
        source_directory: source.path().to_path_buf(),
        manifest_path: None,
        env_file: None,
        create_only: false,
        agent: agent(name),
    };
    ensure(&fixture.control_plane, "builder", Some(&workspace)).await;

    let error = fixture
        .control_plane
        .apply(apply("builder-repo"))
        .await
        .expect_err("apply over a bundled agent");
    assert!(
        matches!(&error, Error::Invalid(message) if message.contains("run by agentd")),
        "{error}"
    );

    let other = fixture.workspace("other");
    fixture
        .control_plane
        .apply(apply("builder-other"))
        .await
        .expect("local agent");
    let ensured = ensure(&fixture.control_plane, "builder", Some(&other)).await;
    assert_eq!(ensured.outcome, EnsureOutcome::Created);
    assert_eq!(
        ensured.agent.metadata.name, "builder-other-2",
        "the local agent keeps its name"
    );
    let local = fixture.store.get_by_name("builder-other").await.expect("local agent");
    assert_eq!(local.origin, Origin::Local);
    assert_eq!(local.agent.metadata.generation, 1);
}

#[tokio::test(flavor = "local")]
async fn a_workspace_that_is_not_one_projects_folder_is_refused() {
    let fixture = Fixture::new();
    let file = fixture.workspaces.path().join("file");
    std::fs::write(&file, "").expect("file");
    let user_home = agent::local::home::user_home_directory().expect("home directory");
    let refused = [
        (Path::new("relative").to_path_buf(), "absolute"),
        (fixture.workspaces.path().join("absent"), "cannot be resolved"),
        (file, "must be a folder"),
        (fixture.home.path().to_path_buf(), "agentd's home"),
        (fixture.home.path().join("bundled"), "agentd's home"),
        (
            fixture
                .home
                .path()
                .parent()
                .expect("parent of agentd's home")
                .to_path_buf(),
            "agentd's home",
        ),
        (user_home, "your home folder"),
        (Path::new("/").to_path_buf(), "home"),
    ];
    std::fs::create_dir_all(fixture.home.path().join("bundled")).expect("bundled folder");
    for (workspace, reason) in refused {
        let error = fixture
            .control_plane
            .ensure(request("builder", Some(&workspace)))
            .await
            .expect_err("refused workspace");
        assert!(
            matches!(&error, Error::Invalid(message) if message.contains(reason)),
            "{}: {error}",
            workspace.display()
        );
    }

    let workspace = fixture.workspace("repo");
    let error = fixture
        .control_plane
        .ensure(request("sign-in", Some(&workspace)))
        .await
        .expect_err("a workspace for a definition that takes none");
    assert!(error.to_string().contains("takes no workspace"), "{error}");
    let error = fixture
        .control_plane
        .ensure(request("builder", None))
        .await
        .expect_err("no workspace for a definition that needs one");
    assert!(error.to_string().contains("needs a workspace"), "{error}");
    std::fs::write(workspace.join(".env"), "TOKEN=secret").expect(".env");
    let error = fixture
        .control_plane
        .ensure(request("builder", Some(&workspace)))
        .await
        .expect_err("a workspace with a secret file");
    assert!(error.to_string().contains(".env"), "{error}");
    assert_eq!(fixture.store.list().await.expect("agents").len(), 0);
}

/// Waits until agentd has asked to delete `name`.
pub(crate) async fn deleting(store: &memory::InMemoryAgentStore, name: &str) -> bool {
    for _ in 0..100 {
        match store.get_by_name(name).await {
            Ok(record) if record.agent.metadata.deletion_timestamp.is_some() => return true,
            Err(Error::NotFound) => return true,
            _ => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    false
}

#[tokio::test(flavor = "local")]
async fn agentd_deletes_the_ephemeral_agents_an_earlier_run_left_when_it_starts() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    let (left, _) = fixture
        .control_plane
        .ensure(request("sign-in", None))
        .await
        .expect("ephemeral agent of a run that crashed");
    ensure(&fixture.control_plane, "builder", Some(&workspace)).await;
    let source = TempDirectory::new("bundled-local");
    fixture
        .control_plane
        .apply(agent::control_plane::ApplyRequest {
            source_directory: source.path().to_path_buf(),
            manifest_path: None,
            env_file: None,
            create_only: false,
            agent: agent("local"),
        })
        .await
        .expect("local agent");
    std::fs::create_dir_all(fixture.home.path().join("bundled/.unfinished")).expect("copy left half-written");

    let restarted = fixture.rebuilt([builder("one"), sign_in()]);
    restarted.remove_leftovers().await.expect("leftovers");

    let records = fixture.store.list().await.expect("agents");
    let deleting = records
        .iter()
        .filter(|record| record.agent.metadata.deletion_timestamp.is_some())
        .map(|record| record.agent.metadata.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(deleting, [left.agent.metadata.name.as_str()]);
    assert!(
        !fixture.home.path().join("bundled/.unfinished").exists(),
        "what no agent uses is removed"
    );
}

#[tokio::test(flavor = "local")]
async fn a_copy_goes_once_no_agent_uses_it() {
    let fixture = Fixture::new();
    let backend = Rc::new(sandbox_memory::Provider::new());
    let reconciler = Reconciler::new(
        fixture.store.clone(),
        sandbox_service(Rc::new(MemoryProvider::new(backend))),
        ProvisioningState::default(),
    )
    .with_snapshots(fixture.snapshots());
    let workspace = fixture.workspace("repo");
    let other = fixture.workspace("other");
    let first = ensure(&fixture.control_plane, "builder", Some(&workspace)).await;
    let second = ensure(&fixture.control_plane, "builder", Some(&other)).await;
    assert_eq!(digest(&first.agent), digest(&second.agent), "one copy for both");
    let id = |name: &str| {
        let store = fixture.store.clone();
        let name = name.to_owned();
        async move { store.get_by_name(&name).await.expect("agent").id }
    };

    fixture.control_plane.delete("builder-repo").await.expect("delete");
    reconciler.reconcile(id("builder-repo").await).await.expect("release");
    assert_eq!(
        fixture.snapshot_folders(),
        [digest(&first.agent)],
        "the other agent still uses it"
    );

    fixture.control_plane.delete("builder-other").await.expect("delete");
    reconciler.reconcile(id("builder-other").await).await.expect("release");
    assert_eq!(fixture.snapshot_folders(), Vec::<String>::new());
}
