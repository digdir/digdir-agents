//! Running agents from definitions, the bundled ones agentd carries among them: `run`, lifetimes,
//! the folders a run gives bind mounts, applying a bundled definition, and agentd's copies of them.

#![allow(clippy::expect_used)]

use std::{collections::BTreeMap, path::Path, rc::Rc, time::Duration};

use agent::{
    Agent, Error, Lifetime, MountSpec, Origin, RunState,
    bundled::{Definition, DefinitionFile, Definitions, Folders},
    control_plane::{
        AgentStore, ApplyRequest, BindSource, ControlPlane, Ran, Reconciler, RunDefinition, RunRequest, memory,
    },
    progress::ProvisioningState,
};
use sandbox::{image::ImageSource, memory as sandbox_memory};

use crate::{
    control_plane::{MemoryProvider, sandbox_service},
    support::{IgnoreNotifications, TempDirectory, agent},
};

const BASE_IMAGE: &str = "ghcr.io/digdir/agentctl-base:ubuntu-26.04";

/// Where the definitions' checkout is mounted, and their sessions start.
const CHECKOUT: &str = "/home/agent/code/checkout";

/// The bind mount of a checkout whose folder is given when the definition runs.
fn checkout() -> MountSpec {
    MountSpec::Bind {
        source: None,
        target: sandbox::SandboxPath::new(CHECKOUT),
        read_only: false,
        optional: false,
    }
}

/// A manifest named `name` on `image`, reading `instructions.md` and a home, that works on a
/// checkout it is given.
fn manifest(name: &str, image: &str) -> Agent {
    let mut manifest = agent(name);
    manifest.spec.sandbox.image = ImageSource::Reference {
        reference: image.into(),
    };
    manifest.spec.sandbox.mounts = vec![checkout()];
    manifest.spec.working_directory = Some(sandbox::SandboxPath::new(CHECKOUT));
    manifest
}

/// The files of a definition named `name`, whose agent reads `instructions` and a home holding
/// `home`, on `image`. One with no checkout when `checkout` is false.
fn files(name: &str, image: &str, instructions: &str, home: &str, checkout: bool) -> BTreeMap<String, DefinitionFile> {
    let mut manifest = manifest(name, image);
    if !checkout {
        manifest.spec.sandbox.mounts.clear();
        manifest.spec.working_directory = None;
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

    fn folders(&self) -> Folders {
        Folders::new(self.home.path().join("bundled"))
    }

    pub(crate) fn workspace(&self, name: &str) -> std::path::PathBuf {
        let path = self.workspaces.path().join(name);
        std::fs::create_dir_all(&path).expect("workspace");
        path
    }

    fn copies(&self) -> Vec<String> {
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
    ControlPlane::new(store.clone(), Rc::new(IgnoreNotifications))
        .with_bundled(Definitions::new(definitions), home.to_path_buf())
}

pub(crate) fn request(definition: &str, workspace: Option<&Path>) -> RunRequest {
    RunRequest {
        definition: RunDefinition::Reference {
            reference: definition.into(),
        },
        binds: workspace.map(given).into_iter().collect(),
        lifetime: None,
        name: None,
    }
}

async fn run(control_plane: &ControlPlane, definition: &str, workspace: Option<&Path>) -> Ran {
    control_plane.run(request(definition, workspace)).await.expect("run").0
}

/// `folder`, given for the one bind mount without a source.
fn given(folder: &Path) -> BindSource {
    BindSource {
        source: folder.to_path_buf(),
        target: None,
    }
}

/// A request to apply the manifest in `directory`.
fn apply(directory: &Path, agent: Agent) -> ApplyRequest {
    ApplyRequest {
        source_directory: directory.to_path_buf(),
        manifest_path: Some(directory.join("agent.yaml")),
        env_file: None,
        binds: Vec::new(),
        create_only: false,
        agent,
    }
}

#[tokio::test(flavor = "local")]
async fn run_makes_the_agent_with_its_folder_from_agentds_copy_of_the_definition() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("Some Repo");

    let ran = run(&fixture.control_plane, "builder", Some(&workspace)).await;

    let agent = ran.agent;
    assert_eq!(agent.metadata.name, "builder-some-repo");
    assert_eq!(agent.spec.run_state(), RunState::Running);
    assert!(
        matches!(&agent.status.origin, Some(Origin::Reference { reference, .. }) if reference == "bundled/builder"),
        "{:?}",
        agent.status.origin
    );
    assert_eq!(agent.status.lifetime, None, "kept until deleted");
    let workspace = workspace.canonicalize().expect("canonical workspace");
    assert_eq!(
        agent.status.bindings,
        BTreeMap::from([(sandbox::SandboxPath::new(CHECKOUT), workspace.clone())])
    );
    assert_eq!(
        agent.spec.sandbox.mounts,
        [MountSpec::Bind {
            source: Some(workspace),
            target: sandbox::SandboxPath::new(CHECKOUT),
            read_only: false,
            optional: false,
        }]
    );

    let copy = fixture.folders().path("builder");
    let provenance = agent.status.provenance.expect("provenance");
    assert_eq!(
        provenance.source_directory, copy,
        "a bundled definition is a folder like any other"
    );
    assert_eq!(
        std::fs::read_to_string(copy.join("instructions.md")).expect("copied instructions"),
        "one"
    );
}

#[tokio::test(flavor = "local")]
async fn concurrent_runs_make_one_agent_for_a_workspace() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    let other = fixture.workspace("other");
    let attempt = || fixture.control_plane.run(request("builder", Some(&workspace)));

    let (first, second, elsewhere) = tokio::join!(
        attempt(),
        attempt(),
        run(&fixture.control_plane, "builder", Some(&other)),
    );

    let (made, refused) = match (first, second) {
        (Ok((made, _)), Err(refused)) | (Err(refused), Ok((made, _))) => (made, refused),
        (first, second) => panic!("one run makes the agent, and the other is refused: {first:?} {second:?}"),
    };
    assert_eq!(made.agent.metadata.name, "builder-repo");
    assert!(refused.to_string().contains("already runs"), "{refused}");
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
async fn running_again_on_the_same_folders_is_refused_with_the_command_that_attaches() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    run(&fixture.control_plane, "builder", Some(&workspace)).await;
    fixture
        .control_plane
        .set_run_state("builder-repo", RunState::Stopped)
        .await
        .expect("stop");
    let stopped = fixture.store.get_by_name("builder-repo").await.expect("stored");

    for reference in ["builder", "bundled/builder"] {
        let error = fixture
            .control_plane
            .run(request(reference, Some(&workspace)))
            .await
            .expect_err("an agent of the definition already works on the folder");
        let message = error.to_string();
        assert!(
            message.contains("agent \"builder-repo\" already runs bundled/builder on ")
                && message.contains("`agentctl attach session/main --agent builder-repo`"),
            "{reference}: {message}"
        );
    }
    let record = fixture.store.get_by_name("builder-repo").await.expect("stored");
    assert_eq!(
        record.agent.metadata.generation, stopped.agent.metadata.generation,
        "running never changes the agent"
    );
    assert_eq!(record.agent.spec.run_state(), RunState::Stopped);

    let (named, _) = fixture
        .control_plane
        .run(RunRequest {
            name: Some("second".into()),
            ..request("builder", Some(&workspace))
        })
        .await
        .expect("a name makes another");
    assert_eq!(named.agent.metadata.name, "second");
}

#[tokio::test(flavor = "local")]
async fn definitions_lists_a_folders_then_the_applied_ones_then_agentds() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    run(&fixture.control_plane, "builder", Some(&workspace)).await;
    let folder = TempDirectory::new("bundled-listed");
    let dev = folder.path().join("agents/dev");
    std::fs::create_dir_all(&dev).expect("definition folder");
    let yaml = |name: &str| serde_yaml_ng::to_string(&agent(name)).expect("manifest");
    std::fs::write(dev.join("agent.yaml"), yaml("dev")).expect("manifest");
    let applied = TempDirectory::new("bundled-applied");
    std::fs::write(applied.path().join("agent.yaml"), yaml("applied")).expect("manifest");
    fixture
        .control_plane
        .apply(apply(applied.path(), agent("applied")))
        .await
        .expect("apply");

    let listed = fixture
        .control_plane
        .definitions(Some(folder.path().to_path_buf()))
        .await
        .expect("definitions");

    let entries = listed
        .iter()
        .map(|definition| {
            (
                definition.reference.as_deref(),
                definition.name.as_deref(),
                definition.slots.len(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        entries,
        [
            (None, Some("dev"), 0),
            (None, Some("applied"), 0),
            (Some("bundled/builder"), Some("builder"), 1),
            (Some("bundled/sign-in"), Some("sign-in"), 0),
        ],
        "the copy the builder's agent was made from is listed once, by its reference"
    );
    assert_eq!(listed[0].path, dev.join("agent.yaml"));
    assert_eq!(listed[1].path, applied.path().join("agent.yaml"));
    let error = fixture
        .control_plane
        .definitions(Some("relative".into()))
        .await
        .expect_err("a relative folder");
    assert!(error.to_string().contains("must be an absolute path"), "{error}");
}

#[tokio::test(flavor = "local")]
async fn an_agent_with_a_bind_mount_without_a_source_is_never_stored() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    run(&fixture.control_plane, "builder", Some(&workspace)).await;
    let mut record = fixture.store.get_by_name("builder-repo").await.expect("stored");
    if let MountSpec::Bind { source, .. } = &mut record.agent.spec.sandbox.mounts[0] {
        *source = None;
    }
    let generation = record.agent.metadata.generation;

    let error = fixture
        .store
        .put(record, generation)
        .await
        .expect_err("a bind mount without a source");

    assert!(
        error.to_string().contains("spec.sandbox.mounts[0] has no source"),
        "{error}"
    );
}

#[tokio::test(flavor = "local")]
async fn a_reference_agentd_cannot_resolve_is_refused() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");

    for (reference, expected) in [
        ("missing", "no agent definition named \"missing\""),
        ("kihub.digdir.no/dotnet-dev", "names a definition on kihub.digdir.no"),
    ] {
        let error = fixture
            .control_plane
            .run(request(reference, Some(&workspace)))
            .await
            .expect_err("an unknown reference");
        assert!(error.to_string().contains(expected), "{reference}: {error}");
    }
    assert_eq!(fixture.store.list().await.expect("agents"), []);
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
        binds: vec![given(&workspace)],
        lifetime: None,
        name: name.map(str::to_owned),
    };

    let (created, _) = fixture.control_plane.run(local(None)).await.expect("run");
    let again = fixture.control_plane.run(local(None)).await.expect_err("run again");
    let (named, _) = fixture.control_plane.run(local(Some("mine"))).await.expect("run named");

    assert_eq!(created.agent.metadata.name, "reviewer-repo");
    assert_eq!(created.agent.status.origin, Some(Origin::Local));
    let manifest = definition.path().join("agent.yaml");
    assert!(
        again
            .to_string()
            .contains(&format!("agent \"reviewer-repo\" already runs {}", manifest.display())),
        "{again}"
    );
    assert_eq!(named.agent.metadata.name, "mine", "a name of its own is another agent");
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
async fn a_newer_build_replaces_agentds_copy_which_its_agents_then_read() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    let created = run(&fixture.control_plane, "builder", Some(&workspace)).await;

    let rebuilt = fixture.rebuilt([builder("two")]);
    rebuilt.remove_leftovers().await.expect("agentd starts");

    let record = fixture.store.get_by_name("builder-repo").await.expect("stored");
    assert_eq!(
        record.agent.metadata.generation, created.agent.metadata.generation,
        "the agent is left as it was"
    );
    assert_eq!(
        std::fs::read_to_string(record.source_directory.join("instructions.md")).expect("new instructions"),
        "two",
        "as an agent applied from a checkout reads what a pull brings"
    );
    assert_eq!(
        fixture.copies(),
        ["builder"],
        "the copy of sign-in goes, as no agent was made from it and this build no longer carries it"
    );
    let error = rebuilt
        .run(request("builder", Some(&workspace)))
        .await
        .expect_err("running it again on the same folder");
    assert!(error.to_string().contains("already runs bundled/builder"), "{error}");
}

#[tokio::test(flavor = "local")]
async fn a_bundled_definition_is_applied_from_agentds_copy_like_any_other() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    let definitions = fixture.control_plane.definitions(None).await.expect("definitions");
    let builder = definitions
        .iter()
        .find(|definition| definition.reference.as_deref() == Some("bundled/builder"))
        .expect("builder");
    let directory = builder.path.parent().expect("the copy's folder");
    assert_eq!(
        builder.slots,
        [sandbox::SandboxPath::new(CHECKOUT)],
        "the list says it needs a folder"
    );
    let copy: Agent = serde_yaml_ng::from_slice(&std::fs::read(&builder.path).expect("manifest")).expect("manifest");

    let error = fixture
        .control_plane
        .apply(apply(directory, copy.clone()))
        .await
        .expect_err("a definition that needs a folder applied without one");
    assert!(error.to_string().contains("names no source"), "{error}");
    let applied = fixture
        .control_plane
        .apply(ApplyRequest {
            binds: vec![given(&workspace)],
            ..apply(directory, copy.clone())
        })
        .await
        .expect("apply the bundled definition");
    assert_eq!(applied.metadata.name, "builder");
    assert_eq!(
        applied.status.origin,
        Some(Origin::Reference {
            reference: "bundled/builder".into()
        })
    );
    let again = fixture
        .control_plane
        .apply(apply(directory, copy.clone()))
        .await
        .expect("an update keeps the agent's folder");
    assert_eq!(again.metadata.generation, applied.metadata.generation);
    assert_eq!(again.spec.sandbox.mounts, applied.spec.sandbox.mounts);

    let source = TempDirectory::new("bundled-local");
    let error = fixture
        .control_plane
        .apply(apply(source.path(), agent("builder")))
        .await
        .expect_err("another definition's manifest with the same name");
    assert!(
        error.to_string().contains("from definition \"bundled/builder\""),
        "{error}"
    );
}

#[tokio::test(flavor = "local")]
async fn a_folder_that_is_not_one_projects_is_refused() {
    let fixture = Fixture::new();
    let file = fixture.workspaces.path().join("file");
    std::fs::write(&file, "").expect("file");
    let user_home = agent::local::home::user_home_directory().expect("home directory");
    // `/`, or the drive's root on Windows.
    let root = user_home.ancestors().last().expect("a root").to_path_buf();
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
        (root, "home"),
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
        .expect_err("a folder for a definition that takes none");
    assert!(error.to_string().contains("no bind mount without a source"), "{error}");
    let error = fixture
        .control_plane
        .run(request("builder", None))
        .await
        .expect_err("no folder for a definition that needs one");
    assert!(error.to_string().contains("names no source"), "{error}");
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
fn an_optional_bind_mount_needs_a_source() {
    let mut agent = agent("worker");
    agent.spec.sandbox.mounts = vec![MountSpec::Bind {
        source: None,
        target: sandbox::SandboxPath::new(CHECKOUT),
        read_only: true,
        optional: true,
    }];

    let error = agent.validate().expect_err("an optional bind mount without a source");

    assert!(error.to_string().contains("needs a source"), "{error}");
}

#[tokio::test(flavor = "local")]
async fn applying_a_manifest_that_now_names_the_given_folder_forgets_it_was_given() {
    let fixture = Fixture::new();
    let definition = TempDirectory::new("bundled-local");
    std::fs::create_dir_all(definition.path().join("checkout")).expect("checkout");
    let worktree = fixture
        .workspace("worktree")
        .canonicalize()
        .expect("canonical worktree");
    let declaring = |source: &Path| {
        let mut manifest = manifest("dev", BASE_IMAGE);
        manifest.spec.sandbox.mounts = vec![MountSpec::Bind {
            source: Some(source.to_path_buf()),
            target: sandbox::SandboxPath::new(CHECKOUT),
            read_only: false,
            optional: false,
        }];
        manifest
    };
    let local = |manifest: Agent, binds: Vec<BindSource>| RunRequest {
        definition: RunDefinition::Local {
            source_directory: definition.path().to_path_buf(),
            manifest_path: definition.path().join("agent.yaml"),
            env_file: None,
            agent: Box::new(manifest),
        },
        binds,
        lifetime: None,
        name: None,
    };
    let (given, _) = fixture
        .control_plane
        .run(local(
            declaring(Path::new("checkout")),
            vec![BindSource {
                source: worktree.clone(),
                target: Some(sandbox::SandboxPath::new(CHECKOUT)),
            }],
        ))
        .await
        .expect("run on the worktree");
    assert_eq!(given.agent.metadata.name, "dev-worktree");

    let mut now = declaring(&worktree);
    now.metadata.name = "dev-worktree".into();
    let applied = fixture
        .control_plane
        .apply(apply(definition.path(), now))
        .await
        .expect("the manifest now names the worktree");

    assert_eq!(applied.status.bindings, BTreeMap::new());
    let error = fixture
        .control_plane
        .run(local(declaring(&worktree), Vec::new()))
        .await
        .expect_err("the same definition on the same folder");
    assert!(error.to_string().contains("already runs"), "{error}");
}

#[tokio::test(flavor = "local")]
async fn a_kept_copy_of_a_definition_the_build_no_longer_carries_is_still_that_definition() {
    let fixture = Fixture::new();
    let (made, _) = fixture
        .control_plane
        .run(request("sign-in", None))
        .await
        .expect("run sign-in");
    let rebuilt = fixture.rebuilt([builder("one")]);
    rebuilt.remove_leftovers().await.expect("agentd starts");
    let copy = fixture.folders().path("sign-in");
    let mut manifest: Agent =
        serde_yaml_ng::from_slice(&std::fs::read(copy.join("agent.yaml")).expect("kept manifest")).expect("manifest");
    manifest.metadata.name.clone_from(&made.agent.metadata.name);

    let applied = rebuilt
        .apply(apply(&copy, manifest))
        .await
        .expect("the agent's own definition");

    assert_eq!(
        applied.status.origin,
        Some(Origin::Reference {
            reference: "bundled/sign-in".into()
        })
    );
}

#[tokio::test(flavor = "local")]
async fn a_folder_given_for_a_bind_mount_replaces_the_manifests_and_makes_another_agent() {
    let fixture = Fixture::new();
    let definition = TempDirectory::new("bundled-local");
    let checkout = definition.path().join("checkout");
    std::fs::create_dir_all(&checkout).expect("checkout");
    let worktree = fixture.workspace("worktree");
    let mut manifest = manifest("dev", BASE_IMAGE);
    manifest.spec.sandbox.mounts = vec![MountSpec::Bind {
        source: Some("checkout".into()),
        target: sandbox::SandboxPath::new(CHECKOUT),
        read_only: false,
        optional: false,
    }];
    let local = |binds: Vec<BindSource>| RunRequest {
        definition: RunDefinition::Local {
            source_directory: definition.path().to_path_buf(),
            manifest_path: definition.path().join("agent.yaml"),
            env_file: None,
            agent: Box::new(manifest.clone()),
        },
        binds,
        lifetime: None,
        name: None,
    };
    let at = |folder: &Path| BindSource {
        source: folder.to_path_buf(),
        target: Some(sandbox::SandboxPath::new(CHECKOUT)),
    };

    let (own, _) = fixture.control_plane.run(local(Vec::new())).await.expect("run");
    let same = fixture
        .control_plane
        .run(local(vec![at(&checkout)]))
        .await
        .expect_err("run on its own folder");
    let (other, _) = fixture
        .control_plane
        .run(local(vec![at(&worktree)]))
        .await
        .expect("run on another");

    assert_eq!(own.agent.metadata.name, "dev");
    assert_eq!(own.agent.status.bindings, BTreeMap::new());
    assert!(
        same.to_string().contains("agent \"dev\" already runs"),
        "the manifest's own folder given is the same: {same}"
    );
    assert_eq!(other.agent.metadata.name, "dev-worktree");
    let worktree = worktree.canonicalize().expect("canonical worktree");
    assert!(
        matches!(&other.agent.spec.sandbox.mounts[..], [MountSpec::Bind { source: Some(source), .. }] if *source == worktree),
        "{:?}",
        other.agent.spec.sandbox.mounts
    );
    let error = fixture
        .control_plane
        .run(local(vec![BindSource {
            source: worktree,
            target: Some(sandbox::SandboxPath::new("/elsewhere")),
        }]))
        .await
        .expect_err("a folder for a target the manifest has no bind mount at");
    assert!(error.to_string().contains("no bind mount at /elsewhere"), "{error}");
}

#[tokio::test(flavor = "local")]
async fn the_agent_is_checked_with_the_folders_it_is_given() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("repo");
    let definition = TempDirectory::new("bundled-hidden");
    let mut hidden = manifest("hidden", BASE_IMAGE);
    hidden.spec.sandbox.resources.root_filesystem =
        sandbox::RootFilesystem::direct("4Gi".parse::<sandbox::ByteQuantity>().expect("capacity"));
    hidden.spec.sandbox.mounts.push(MountSpec::Volume {
        volume: "code".into(),
        target: sandbox::SandboxPath::new("/home/agent/code"),
        read_only: false,
        optional: false,
    });

    let error = fixture
        .control_plane
        .apply(ApplyRequest {
            binds: vec![given(&workspace)],
            ..apply(definition.path(), hidden)
        })
        .await
        .expect_err("a writable volume over the given folder");

    assert!(error.to_string().contains("which would hide it"), "{error}");
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
    assert_eq!(
        fixture.copies(),
        ["builder", "sign-in"],
        "copies of what this build carries stay"
    );
}

#[tokio::test(flavor = "local")]
async fn a_copy_the_build_no_longer_carries_stays_while_an_agent_was_made_from_it() {
    let fixture = Fixture::new();
    let backend = Rc::new(sandbox_memory::Provider::new());
    let reconciler = Reconciler::new(
        fixture.store.clone(),
        sandbox_service(Rc::new(MemoryProvider::new(backend))),
        ProvisioningState::default(),
    );
    let workspace = fixture.workspace("repo");
    run(&fixture.control_plane, "builder", Some(&workspace)).await;
    let rebuilt = fixture.rebuilt([sign_in()]);

    rebuilt.remove_leftovers().await.expect("agentd starts");
    assert_eq!(fixture.copies(), ["builder", "sign-in"], "an agent was made from it");

    fixture.control_plane.delete("builder-repo").await.expect("delete");
    let id = fixture.store.get_by_name("builder-repo").await.expect("agent").id;
    reconciler.reconcile(id).await.expect("release");
    rebuilt.remove_leftovers().await.expect("agentd starts again");
    assert_eq!(fixture.copies(), ["sign-in"]);
}
