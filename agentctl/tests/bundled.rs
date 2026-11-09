//! Running agents from definitions, the bundled ones agentd carries among them: `run`, lifetimes,
//! workspaces, applying a bundled definition, and agentd's copies of them.

#![allow(clippy::expect_used)]

use std::{collections::BTreeMap, path::Path, rc::Rc, time::Duration};

use agent::{
    Agent, Error, Lifetime, MountSpec, Origin, RunState, WorkspaceSpec,
    bundled::{Definition, DefinitionFile, Definitions, Snapshots},
    control_plane::{
        AgentStore, ApplyRequest, ControlPlane, Ran, Reconciler, RunDefinition, RunOutcome, RunRequest, memory,
    },
    progress::ProvisioningState,
};
use sandbox::{image::ImageSource, memory as sandbox_memory};

use crate::{
    control_plane::{MemoryProvider, sandbox_service},
    support::{IgnoreNotifications, TempDirectory, agent},
};

const BASE_IMAGE: &str = "ghcr.io/digdir/agentctl-base:ubuntu-26.04";

/// A manifest named `name` on `image`, reading `instructions.md` and a home, that takes a workspace
/// it may change only `agents/` of.
fn manifest(name: &str, image: &str) -> Agent {
    let mut manifest = agent(name);
    manifest.spec.sandbox.image = ImageSource::Reference {
        reference: image.into(),
    };
    manifest.spec.workspace = Some(WorkspaceSpec {
        read_only: true,
        writable: vec!["agents".into()],
    });
    manifest
}

/// The files of a definition named `name`, whose agent reads `instructions` and a home holding
/// `home`, on `image`. One without a workspace when `workspace` is false.
fn files(name: &str, image: &str, instructions: &str, home: &str, workspace: bool) -> BTreeMap<String, DefinitionFile> {
    let mut manifest = manifest(name, image);
    if !workspace {
        manifest.spec.workspace = None;
    }
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
    Definition::new("builder", files("builder", image, instructions, home, true)).expect("builder definition")
}

fn sign_in() -> Definition {
    Definition::new("sign-in", files("sign-in", BASE_IMAGE, "sign in", "home", false)).expect("sign-in definition")
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
    let definitions = Definitions::new(definitions);
    let snapshots = Snapshots::new(home.join("bundled")).carrying(&definitions);
    ControlPlane::new(store.clone(), Rc::new(IgnoreNotifications)).with_bundled(
        definitions,
        snapshots,
        home.to_path_buf(),
    )
}

pub(crate) fn request(definition: &str, workspace: Option<&Path>) -> RunRequest {
    RunRequest {
        definition: RunDefinition::Bundled {
            name: definition.into(),
        },
        workspace: workspace.map(Path::to_path_buf),
        lifetime: None,
        name: None,
    }
}

async fn run(control_plane: &ControlPlane, definition: &str, workspace: Option<&Path>) -> Ran {
    control_plane.run(request(definition, workspace)).await.expect("run").0
}

fn digest(agent: &Agent) -> String {
    match agent.status.origin.as_ref() {
        Some(Origin::Bundled { digest, .. }) => Some(digest.clone()),
        _ => None,
    }
    .expect("a bundled agent")
}

/// A request to apply the manifest in `directory`.
fn apply(directory: &Path, agent: Agent) -> ApplyRequest {
    ApplyRequest {
        source_directory: directory.to_path_buf(),
        manifest_path: Some(directory.join("agent.yaml")),
        env_file: None,
        workspace: None,
        create_only: false,
        agent,
    }
}

#[tokio::test(flavor = "local")]
async fn run_makes_the_agent_with_its_workspace_from_agentds_copy_of_the_definition() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("Some Repo");

    let ran = run(&fixture.control_plane, "builder", Some(&workspace)).await;

    assert_eq!(ran.outcome, RunOutcome::Created);
    let agent = ran.agent;
    assert_eq!(agent.metadata.name, "builder-some-repo");
    assert_eq!(agent.spec.run_state(), RunState::Running);
    assert!(
        matches!(&agent.status.origin, Some(Origin::Bundled { definition, .. }) if definition == "builder"),
        "{:?}",
        agent.status.origin
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

    let snapshot = fixture.snapshots().path(&digest(&agent));
    let provenance = agent.status.provenance.expect("provenance");
    assert_eq!(
        provenance.source_directory, snapshot,
        "a bundled definition is a folder like any other"
    );
    assert_eq!(
        std::fs::read_to_string(snapshot.join("instructions.md")).expect("copied instructions"),
        "one"
    );
}

#[tokio::test(flavor = "local")]
async fn concurrent_runs_make_one_agent_for_a_workspace() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    let other = fixture.workspace("other");

    let (first, second, elsewhere) = tokio::join!(
        run(&fixture.control_plane, "builder", Some(&workspace)),
        run(&fixture.control_plane, "builder", Some(&workspace)),
        run(&fixture.control_plane, "builder", Some(&other)),
    );

    let mut outcomes = [first.outcome, second.outcome];
    outcomes.sort_by_key(|outcome| format!("{outcome:?}"));
    assert_eq!(outcomes, [RunOutcome::Created, RunOutcome::Resumed]);
    assert_eq!(first.agent.metadata.name, second.agent.metadata.name);
    assert_eq!(elsewhere.outcome, RunOutcome::Created);
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
async fn running_again_resumes_the_agent_and_starts_it() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    let created = run(&fixture.control_plane, "builder", Some(&workspace)).await;
    fixture
        .control_plane
        .set_run_state("builder-repo", RunState::Stopped)
        .await
        .expect("stop");

    let resumed = run(&fixture.control_plane, "builder", Some(&workspace)).await;

    assert_eq!(resumed.outcome, RunOutcome::Resumed);
    assert_eq!(resumed.agent.metadata.name, created.agent.metadata.name);
    assert_eq!(resumed.agent.spec.run_state(), RunState::Running);
    let again = run(&fixture.control_plane, "builder", Some(&workspace)).await;
    assert_eq!(again.outcome, RunOutcome::Resumed);
    assert_eq!(
        again.agent.metadata.generation, resumed.agent.metadata.generation,
        "a running agent of the same definition is left as it is"
    );
}

#[tokio::test(flavor = "local")]
async fn a_definition_on_disk_runs_like_a_bundled_one() {
    let fixture = Fixture::new();
    let definition = TempDirectory::new("bundled-local");
    let workspace = fixture.workspace("repo");
    let local = |name: Option<&str>| RunRequest {
        definition: RunDefinition::Local {
            source_directory: definition.path().to_path_buf(),
            manifest_path: definition.path().join("agent.yaml"),
            env_file: None,
            agent: Box::new(manifest("reviewer", BASE_IMAGE)),
        },
        workspace: Some(workspace.clone()),
        lifetime: None,
        name: name.map(str::to_owned),
    };

    let (created, _) = fixture.control_plane.run(local(None)).await.expect("run");
    let (resumed, _) = fixture.control_plane.run(local(None)).await.expect("run again");
    let (named, _) = fixture.control_plane.run(local(Some("mine"))).await.expect("run named");

    assert_eq!(created.outcome, RunOutcome::Created);
    assert_eq!(created.agent.metadata.name, "reviewer-repo");
    assert_eq!(created.agent.status.origin, Some(Origin::Local));
    assert_eq!(resumed.outcome, RunOutcome::Resumed);
    assert_eq!(resumed.agent.metadata.name, "reviewer-repo");
    assert_eq!(named.outcome, RunOutcome::Created, "a name of its own is another agent");
    assert_eq!(named.agent.metadata.name, "mine");
    let error = fixture
        .control_plane
        .run(RunRequest {
            name: Some("mine".into()),
            ..request("builder", Some(&workspace))
        })
        .await
        .expect_err("a name another definition's agent has");
    assert!(error.to_string().contains("already exists"), "{error}");
}

#[tokio::test(flavor = "local")]
async fn a_newer_definition_updates_the_agent_in_place_and_its_old_copy_goes() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    let created = run(&fixture.control_plane, "builder", Some(&workspace)).await;

    let rebuilt = fixture.rebuilt([builder("two")]);
    let updated = run(&rebuilt, "builder", Some(&workspace)).await;

    assert_eq!(updated.outcome, RunOutcome::Updated);
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
        "no agent uses the old copy, and this build no longer carries it"
    );
}

#[tokio::test(flavor = "local")]
async fn a_definition_that_changes_what_cannot_change_in_place_reports_the_agent_outdated() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    let created = run(&fixture.control_plane, "builder", Some(&workspace)).await;

    for (definition, field) in [
        (
            builder_with("ghcr.io/digdir/agentctl-base:ubuntu-26.04-desktop", "one", "home"),
            "spec.sandbox.image",
        ),
        (builder_with(BASE_IMAGE, "one", "a new home"), "spec.home"),
    ] {
        let rebuilt = fixture.rebuilt([definition]);
        let outdated = rebuilt
            .run(request("builder", Some(&workspace)))
            .await
            .expect("an outdated agent is an outcome, not an error")
            .0;

        let RunOutcome::Outdated { reason } = outdated.outcome else {
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
async fn a_bundled_definition_is_applied_from_agentds_copy_like_any_other() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    let definitions = fixture.control_plane.bundled_definitions().await.expect("definitions");
    let builder = definitions
        .iter()
        .find(|definition| definition.name == "builder")
        .expect("builder");
    let copy: Agent = serde_yaml_ng::from_slice(&std::fs::read(builder.path.join("agent.yaml")).expect("manifest"))
        .expect("manifest");

    let error = fixture
        .control_plane
        .apply(apply(&builder.path, copy.clone()))
        .await
        .expect_err("a workspace definition applied without one");
    assert!(error.to_string().contains("needs a workspace"), "{error}");
    let applied = fixture
        .control_plane
        .apply(ApplyRequest {
            workspace: Some(workspace.clone()),
            ..apply(&builder.path, copy.clone())
        })
        .await
        .expect("apply the bundled definition");
    assert_eq!(applied.metadata.name, "builder");
    assert_eq!(digest(&applied), builder.digest);
    let again = fixture
        .control_plane
        .apply(apply(&builder.path, copy.clone()))
        .await
        .expect("an update keeps the agent's workspace");
    assert_eq!(again.metadata.generation, applied.metadata.generation);
    assert_eq!(again.spec.sandbox.mounts, applied.spec.sandbox.mounts);

    let source = TempDirectory::new("bundled-local");
    let error = fixture
        .control_plane
        .apply(apply(source.path(), agent("builder")))
        .await
        .expect_err("another definition's manifest with the same name");
    assert!(error.to_string().contains("agentd's definition \"builder\""), "{error}");
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
            .run(request("builder", Some(&workspace)))
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
        .run(request("sign-in", Some(&workspace)))
        .await
        .expect_err("a workspace for a definition that takes none");
    assert!(error.to_string().contains("takes no workspace"), "{error}");
    let error = fixture
        .control_plane
        .run(request("builder", None))
        .await
        .expect_err("no workspace for a definition that needs one");
    assert!(error.to_string().contains("needs a workspace"), "{error}");
    std::fs::write(workspace.join(".env"), "TOKEN=secret").expect(".env");
    let error = fixture
        .control_plane
        .run(request("builder", Some(&workspace)))
        .await
        .expect_err("a workspace with a secret file");
    assert!(error.to_string().contains(".env"), "{error}");
    assert_eq!(fixture.store.list().await.expect("agents").len(), 0);
}

#[test]
fn only_folders_inside_a_read_only_workspace_can_be_writable() {
    for (workspace, problem) in [
        (
            WorkspaceSpec {
                read_only: false,
                writable: vec!["agents".into()],
            },
            "needs readOnly",
        ),
        (
            WorkspaceSpec {
                read_only: true,
                writable: vec!["../elsewhere".into()],
            },
            "inside the workspace",
        ),
        (
            WorkspaceSpec {
                read_only: true,
                writable: vec!["/agents".into()],
            },
            "inside the workspace",
        ),
    ] {
        let mut agent = agent("worker");
        agent.spec.workspace = Some(workspace);
        let error = agent.validate().expect_err("invalid workspace");
        assert!(error.to_string().contains(problem), "{error}");
    }
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
        .run(RunRequest {
            lifetime: Some(Lifetime::Ephemeral),
            ..request("sign-in", None)
        })
        .await
        .expect("ephemeral agent of a run that crashed");
    run(&fixture.control_plane, "builder", Some(&workspace)).await;
    let source = TempDirectory::new("bundled-local");
    fixture
        .control_plane
        .apply(apply(source.path(), agent("local")))
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
    let carried = restarted.bundled_definitions().await.expect("definitions");
    let mut digests = carried
        .into_iter()
        .map(|definition| definition.digest)
        .collect::<Vec<_>>();
    digests.sort();
    assert_eq!(
        fixture.snapshot_folders(),
        digests,
        "copies of what this build carries stay"
    );
}

#[tokio::test(flavor = "local")]
async fn a_copy_goes_once_no_agent_uses_it_and_the_build_no_longer_carries_it() {
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
    let first = run(&fixture.control_plane, "builder", Some(&workspace)).await;
    let second = run(&fixture.control_plane, "builder", Some(&other)).await;
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
    assert_eq!(
        fixture.snapshot_folders(),
        Vec::<String>::new(),
        "this reconciler's build carries none"
    );
}
