#![allow(clippy::expect_used)]

use std::{path::PathBuf, rc::Rc, time::Duration};

use agent::{
    Condition, ConditionStatus, MountSpec,
    control_plane::{AgentStore, ApplyRequest, ControlPlane, Notifier, memory},
    progress::ProvisioningState,
    sandbox::Provider,
    volumes::{
        ApplyVolumeRequest, Reconciler, Volume, VolumeService, VolumeSpec, VolumeStore as _,
        memory::InMemoryVolumeStore,
    },
};
use sandbox::{SandboxPath, backend::SandboxBackend as _, memory as sandbox_memory, mount::Mount};

use crate::{
    control_plane::{MemoryProvider, sandbox_service},
    support::{TempDirectory, agent},
};

pub(crate) struct VolumeFixture {
    pub(crate) agents: Rc<memory::InMemoryAgentStore>,
    pub(crate) volumes: Rc<InMemoryVolumeStore>,
    pub(crate) backend: Rc<sandbox_memory::Provider>,
    pub(crate) provider: Rc<MemoryProvider>,
    pub(crate) service: VolumeService,
    pub(crate) reconciler: Reconciler,
    pub(crate) control_plane: ControlPlane,
    pub(crate) agent_reconciler: agent::control_plane::Reconciler,
    /// agentd's home directory.
    pub(crate) home: TempDirectory,
    _controller: agent::volumes::Controller,
}

struct Unscheduled;

impl Notifier for Unscheduled {
    fn notify(&self, _id: agent::AgentId) {}
}

pub(crate) fn volume_fixture() -> VolumeFixture {
    let agents = Rc::new(memory::InMemoryAgentStore::new());
    let volumes = Rc::new(InMemoryVolumeStore::new());
    let backend = Rc::new(sandbox_memory::Provider::new());
    let provider = Rc::new(MemoryProvider::new(backend.clone()));
    let sandboxes = sandbox_service(provider.clone() as Rc<dyn Provider>);
    let progress = agent::volumes::CopyProgress::default();
    let revisions = agent::volumes::VolumeRevisions::new(volumes.clone());
    let reconciler =
        Reconciler::new(revisions.clone(), agents.clone(), sandboxes.clone()).with_progress(progress.clone());
    let agent_reconciler =
        agent::control_plane::Reconciler::new(agents.clone(), sandboxes.clone(), ProvisioningState::default())
            .with_volumes(revisions.clone());
    let (controller, wakeup) = agent::volumes::Controller::new(
        volumes.clone(),
        Rc::new(Reconciler::new(revisions, agents.clone(), sandboxes).with_progress(progress.clone())),
        Duration::from_secs(3600),
        Rc::new(|_, _| {}),
    );
    let home = TempDirectory::new("volume-agent-home");
    VolumeFixture {
        service: VolumeService::new(volumes.clone(), agents.clone(), wakeup)
            .with_progress(progress)
            .with_home(home.path().to_path_buf()),
        home,
        control_plane: ControlPlane::new(agents.clone(), Rc::new(Unscheduled)).with_volumes(volumes.clone()),
        agent_reconciler,
        agents,
        volumes,
        backend,
        provider,
        reconciler,
        _controller: controller,
    }
}

pub(crate) fn volume_request(name: &str, source: PathBuf) -> ApplyVolumeRequest {
    ApplyVolumeRequest {
        manifest_path: None,
        source_directory: std::env::temp_dir(),
        volume: Volume {
            api_version: agent::API_VERSION.into(),
            kind: agent::volumes::KIND.into(),
            metadata: agent::Metadata {
                name: name.into(),
                generation: 0,
                deletion_timestamp: None,
                restart_timestamp: None,
            },
            spec: VolumeSpec::Copy { source },
            status: agent::volumes::VolumeStatus::default(),
        },
    }
}

pub(crate) async fn reconcile_volume(fixture: &VolumeFixture, name: &str) {
    let id = fixture.volumes.get_by_name(name).await.expect("stored volume").id;
    fixture.reconciler.reconcile(id).await.expect("reconcile volume");
}

/// The volume's revision numbers, and the copy being taken or failed: its number and whether it failed.
fn copies(volume: &Volume) -> (Vec<u64>, Option<(u64, bool)>) {
    (
        volume.status.revisions.iter().map(|revision| revision.number).collect(),
        volume
            .status
            .copy
            .as_ref()
            .map(|copy| (copy.number, copy.failure.is_some())),
    )
}

#[tokio::test(flavor = "local")]
async fn agentd_lists_a_folders_volume_manifests_then_the_ones_volumes_were_applied_from() {
    let fixture = volume_fixture();
    let folder = TempDirectory::new("volume-manifests-folder");
    let shared = folder.path().join("agents/_volumes");
    std::fs::create_dir_all(&shared).expect("volumes folder");
    let manifest = |name: &str| {
        format!(
            "apiVersion: agents.platform/v1alpha1\nkind: Volume\nmetadata:\n  name: {name}\nspec:\n  type: copy\n  source: /data/{name}\n"
        )
    };
    std::fs::write(shared.join("docs.yaml"), manifest("docs")).expect("manifest in the folder");
    let elsewhere = TempDirectory::new("volume-manifests-applied");
    let applied = elsewhere.path().join("logs.yaml");
    std::fs::write(&applied, manifest("logs")).expect("applied manifest");
    let source = TempDirectory::new("volume-manifests-source");
    fixture
        .service
        .apply(ApplyVolumeRequest {
            manifest_path: Some(applied.clone()),
            ..volume_request("logs", source.path().to_path_buf())
        })
        .await
        .expect("apply");

    let listed = fixture
        .service
        .manifests(Some(folder.path().to_path_buf()))
        .await
        .expect("manifests");

    let found = listed
        .iter()
        .map(|manifest| (manifest.path.clone(), manifest.name.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        found,
        [
            (shared.join("docs.yaml"), Some("docs".to_owned())),
            (applied, Some("logs".to_owned())),
        ]
    );
    let error = fixture
        .service
        .manifests(Some("relative".into()))
        .await
        .expect_err("a relative folder");
    assert!(error.to_string().contains("must be an absolute path"), "{error}");
}

#[tokio::test(flavor = "local")]
async fn applying_a_copy_volume_takes_a_new_revision_only_when_its_source_changed() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-source");
    let source = temporary.path().join("apps");
    std::fs::create_dir_all(source.join("app")).expect("source");
    std::fs::write(source.join("app/file.txt"), b"one").expect("file");

    let applied = fixture
        .service
        .apply(volume_request("apps", source.clone()))
        .await
        .expect("apply");
    let VolumeSpec::Copy { source: resolved } = &applied.spec;
    assert_eq!(resolved, &std::fs::canonicalize(&source).expect("canonical source"));
    assert_eq!(copies(&applied), (vec![], Some((1, false))));
    reconcile_volume(&fixture, "apps").await;
    let ready = fixture.service.get("apps").await.expect("get");
    assert_eq!(copies(&ready), (vec![1], None));
    assert_eq!(
        (ready.status.revisions[0].files, ready.status.revisions[0].bytes),
        (1, 3)
    );

    let unchanged = fixture
        .service
        .apply(volume_request("apps", source.clone()))
        .await
        .expect("apply unchanged");
    assert_eq!(copies(&unchanged), (vec![1], None));

    let manifest = temporary.path().join("agents/_volumes/apps.yaml");
    let mut from_manifest = volume_request("apps", source.clone());
    from_manifest.manifest_path = Some(manifest.clone());
    let recorded = fixture
        .service
        .apply(from_manifest)
        .await
        .expect("apply from a manifest");
    assert_eq!(
        copies(&recorded),
        (vec![1], None),
        "applying from another manifest takes no copy of an unchanged source"
    );
    assert_eq!(
        recorded.status.manifest_path(),
        Some(manifest.as_path()),
        "the manifest is reported so it can be applied again"
    );

    std::fs::write(source.join("app/new.txt"), b"two").expect("new file");
    let changed = fixture
        .service
        .apply(volume_request("apps", source.clone()))
        .await
        .expect("apply changed");
    assert_eq!(copies(&changed), (vec![1], Some((2, false))));
    reconcile_volume(&fixture, "apps").await;
    let replaced = fixture.service.get("apps").await.expect("get");
    assert_eq!(
        copies(&replaced),
        (vec![2], None),
        "a revision that is not the latest and that no agent uses goes"
    );
    assert_eq!(replaced.status.latest().map(|revision| revision.files), Some(2));
}

#[tokio::test(flavor = "local")]
async fn a_copy_taken_while_its_source_changed_fails_and_applying_again_retries() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-changing");
    let source = temporary.path().join("apps");
    std::fs::create_dir_all(&source).expect("source");
    let changing = source.clone();
    *fixture.provider.while_copying.borrow_mut() = Some(Box::new(move || {
        std::fs::write(changing.join("synced.txt"), b"late").expect("change while copying");
    }));
    fixture
        .service
        .apply(volume_request("apps", source.clone()))
        .await
        .expect("apply");
    reconcile_volume(&fixture, "apps").await;
    let failed = fixture.service.get("apps").await.expect("get");
    assert_eq!(copies(&failed), (vec![], Some((1, true))));
    let failure = failed.status.copy.and_then(|copy| copy.failure).unwrap_or_default();
    assert!(failure.contains("changed after the volume was applied"), "{failure}");
    assert_eq!(
        fixture.backend.volume_count(),
        0,
        "the copy that may mix files is deleted"
    );

    *fixture.provider.while_copying.borrow_mut() = None;
    fixture
        .service
        .apply(volume_request("apps", source))
        .await
        .expect("apply again");
    reconcile_volume(&fixture, "apps").await;
    assert_eq!(
        copies(&fixture.service.get("apps").await.expect("get")),
        (vec![1], None),
        "applying again retries the copy that failed"
    );
}

/// The copy is checked against the source the volume was applied with, so a source that changes
/// before the copy starts fails it too, rather than giving a revision that holds other files than
/// it reports.
#[tokio::test(flavor = "local")]
async fn a_source_that_changes_before_its_copy_starts_fails_the_copy() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-changed-before");
    let source = temporary.path().join("apps");
    std::fs::create_dir_all(&source).expect("source");
    fixture
        .service
        .apply(volume_request("apps", source.clone()))
        .await
        .expect("apply");
    std::fs::write(source.join("late.txt"), b"late").expect("change before the copy");
    reconcile_volume(&fixture, "apps").await;
    assert_eq!(
        copies(&fixture.service.get("apps").await.expect("get")),
        (vec![], Some((1, true)))
    );
}

/// A source can match the latest revision again after a newer copy failed, such as when a file it
/// could not read is readable again. Applying it then clears the failure rather than copying the
/// same files again.
#[tokio::test(flavor = "local")]
async fn applying_a_source_that_matches_the_latest_revision_again_clears_a_failed_copy() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-restored");
    let source = temporary.path().join("apps");
    std::fs::create_dir_all(&source).expect("source");
    fixture
        .service
        .apply(volume_request("apps", source.clone()))
        .await
        .expect("apply");
    reconcile_volume(&fixture, "apps").await;
    let mut record = fixture.volumes.get_by_name("apps").await.expect("stored volume");
    let latest = record.volume.status.revisions[0].clone();
    record.volume.status.copy = Some(agent::volumes::CopyStatus {
        number: 2,
        created: latest.created,
        fingerprint: "unreadable".into(),
        files: latest.files,
        bytes: latest.bytes,
        failure: Some("cannot read a file".into()),
        progress: None,
    });
    let version = record.version;
    fixture.volumes.put(record, version).await.expect("failed copy");

    let restored = fixture
        .service
        .apply(volume_request("apps", source))
        .await
        .expect("apply again");
    assert_eq!(copies(&restored), (vec![1], None));
}

/// Two folders can declare a volume with the same name, such as `docs` in two repositories. The
/// second is refused rather than pointing the first one's agents at its source.
#[tokio::test(flavor = "local")]
async fn a_volume_named_like_one_applied_from_another_folder_is_refused() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-clash");
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    for directory in [&first, &second] {
        std::fs::create_dir_all(directory.join("docs")).expect("source");
    }
    let mut request = volume_request("docs", PathBuf::from("docs"));
    request.source_directory.clone_from(&first);
    fixture.service.apply(request).await.expect("apply");

    let mut elsewhere = volume_request("docs", PathBuf::from("docs"));
    elsewhere.source_directory.clone_from(&second);
    let refused = fixture
        .service
        .apply(elsewhere)
        .await
        .expect_err("another folder's volume");
    assert!(
        refused.to_string().contains("already applied from") && refused.to_string().contains("--name"),
        "{refused}"
    );
    let VolumeSpec::Copy { source } = fixture.service.get("docs").await.expect("get").spec;
    assert_eq!(
        source,
        std::fs::canonicalize(first.join("docs")).expect("canonical source")
    );
}

fn phases(progress: Option<&agent::progress::Provisioning>) -> Vec<(String, sandbox::progress::Outcome)> {
    progress
        .expect("progress of the latest copy")
        .progress
        .finished()
        .iter()
        .map(|phase| (phase.phase.label.to_string(), phase.outcome))
        .collect()
}

/// A copy reports checking its source, copying it and checking it again, so clients can follow
/// it. Only the latest copy keeps its progress, on the copy while it is taken or failed and on the
/// revision it becomes.
#[tokio::test(flavor = "local")]
async fn a_copy_reports_its_progress_on_the_revision_it_takes() {
    use sandbox::progress::{OperationStatus, Outcome};

    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-progress");
    let source = temporary.path().join("apps");
    std::fs::create_dir_all(&source).expect("source");
    let applied = fixture
        .service
        .apply(volume_request("apps", source.clone()))
        .await
        .expect("apply");
    assert!(
        applied.status.copy.as_ref().is_some_and(|copy| copy.progress.is_none()),
        "the copy has not started"
    );
    reconcile_volume(&fixture, "apps").await;
    let ready = fixture.service.get("apps").await.expect("get");
    let revision = &ready.status.revisions[0];
    assert_eq!(
        revision.progress.as_ref().map(|copy| copy.progress.status()),
        Some(&OperationStatus::Succeeded)
    );
    assert_eq!(
        phases(revision.progress.as_ref()),
        [
            ("Check source".to_owned(), Outcome::Completed),
            ("Copy source".to_owned(), Outcome::Completed),
            ("Check source is unchanged".to_owned(), Outcome::Completed),
        ]
    );

    let changing = source.clone();
    *fixture.provider.while_copying.borrow_mut() = Some(Box::new(move || {
        std::fs::write(changing.join("late.txt"), b"late").expect("change while copying");
    }));
    std::fs::write(source.join("new.txt"), b"new").expect("change");
    fixture
        .service
        .apply(volume_request("apps", source))
        .await
        .expect("apply changed");
    reconcile_volume(&fixture, "apps").await;
    let failed = fixture.service.get("apps").await.expect("get");
    assert!(
        failed.status.revisions[0].progress.is_none(),
        "an older copy's progress goes"
    );
    let copy = failed.status.copy.as_ref().expect("failed copy");
    assert!(matches!(
        copy.progress.as_ref().map(|copy| copy.progress.status()),
        Some(OperationStatus::Failed { .. })
    ));
    assert_eq!(
        phases(copy.progress.as_ref()).last(),
        Some(&("Check source is unchanged".to_owned(), Outcome::Failed))
    );
}

#[tokio::test(flavor = "local")]
async fn a_copy_volume_refuses_a_source_holding_a_dot_env_file() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-secrets");
    let source = temporary.path().join("apps");
    std::fs::create_dir_all(source.join("app")).expect("source");
    std::fs::write(source.join("app/.env"), b"PRIVATE=value\n").expect(".env");
    let error = fixture
        .service
        .apply(volume_request("apps", source))
        .await
        .expect_err("the copy would hold .env");
    assert!(error.to_string().contains("copies"), "{error}");
    assert_eq!(fixture.service.list().await.expect("list"), []);
}

/// agentd's home holds its database and the copies themselves.
#[tokio::test(flavor = "local")]
async fn a_copy_volume_refuses_a_source_overlapping_agentds_home() {
    let fixture = volume_fixture();
    let inner = fixture.home.path().join("microsandbox");
    std::fs::create_dir_all(&inner).expect("inner directory");
    for source in [fixture.home.path().to_path_buf(), inner] {
        let error = fixture
            .service
            .apply(volume_request("apps", source.clone()))
            .await
            .expect_err("a copy of agentd's home");
        assert!(
            error.to_string().contains("overlaps agentd's home"),
            "{}: {error}",
            source.display()
        );
    }
    assert_eq!(fixture.service.list().await.expect("list"), []);
}

#[tokio::test(flavor = "local")]
async fn a_deleted_volume_goes_with_its_last_revision() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-delete");
    let source = temporary.path().join("apps");
    std::fs::create_dir_all(&source).expect("source");
    fixture
        .service
        .apply(volume_request("apps", source.clone()))
        .await
        .expect("apply");
    reconcile_volume(&fixture, "apps").await;
    assert_eq!(fixture.backend.volume_count(), 1);

    fixture.service.delete("apps").await.expect("delete");
    let error = fixture
        .service
        .apply(volume_request("apps", source))
        .await
        .expect_err("a volume being deleted is not applied");
    assert!(error.to_string().contains("being deleted"), "{error}");
    reconcile_volume(&fixture, "apps").await;
    assert!(matches!(
        fixture.service.get("apps").await,
        Err(agent::Error::VolumeNotFound(_))
    ));
    assert_eq!(fixture.backend.volume_count(), 0);
    let _ = fixture.agents.list().await.expect("agents");
}

/// agentd may stop after the Provider saved a copy but before agentd recorded it. Deleting the
/// volume then deletes that copy too.
#[tokio::test(flavor = "local")]
async fn deleting_a_volume_deletes_a_copy_an_interruption_left_unrecorded() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-interrupted-delete");
    let source = temporary.path().join("apps");
    std::fs::create_dir_all(&source).expect("source");
    fixture
        .service
        .apply(volume_request("apps", source.clone()))
        .await
        .expect("apply");
    let id = fixture.volumes.get_by_name("apps").await.expect("volume").id;
    let name = sandbox::volume::VolumeName::new(format!("copy-{}-1", id.as_uuid().simple())).expect("name");
    fixture
        .provider
        .ensure_volume(
            sandbox::volume::EnsureVolumeRequest::new(name).with_contents(
                sandbox::volume::VolumeContents::DirectoryCopy {
                    source,
                    owner: sandbox::volume::Owner::default(),
                },
            ),
            sandbox::ProgressReporter::from_callback(|_| {}),
        )
        .await
        .expect("the copy taken before agentd stopped");

    fixture.service.delete("apps").await.expect("delete");
    reconcile_volume(&fixture, "apps").await;
    assert!(matches!(
        fixture.service.get("apps").await,
        Err(agent::Error::VolumeNotFound(_))
    ));
    assert_eq!(fixture.backend.volume_count(), 0);
}

fn mounting_agent(name: &str, volume: &str) -> ApplyRequest {
    let mut agent = agent(name);
    agent.spec.sandbox.mounts.push(MountSpec::Volume {
        volume: volume.into(),
        target: SandboxPath::new("/home/agent/code/apps"),
        read_only: true,
        optional: false,
    });
    ApplyRequest {
        source_directory: std::env::temp_dir().join("agent-platform-source"),
        manifest_path: None,
        env_file: None,
        binds: Vec::new(),
        create_only: false,
        agent,
    }
}

async fn reconcile_agent(fixture: &VolumeFixture, name: &str) -> agent::control_plane::AgentRecord {
    let id = fixture.agents.get_by_name(name).await.expect("stored agent").id;
    fixture.agent_reconciler.reconcile(id).await.expect("reconcile agent");
    fixture.agents.get(id).await.expect("reconciled agent")
}

fn ready_reason(record: &agent::control_plane::AgentRecord) -> (ConditionStatus, String, String) {
    let ready = Condition::find_ready(&record.agent.status.conditions).expect("Ready condition");
    (ready.status, ready.reason.clone(), ready.message.clone())
}

fn revision_agents(volume: &Volume) -> Vec<(u64, Vec<String>)> {
    volume
        .status
        .revisions
        .iter()
        .map(|revision| (revision.number, revision.agents.clone()))
        .collect()
}

/// The revision the agent's first volume mount uses.
fn pinned(record: &agent::control_plane::AgentRecord) -> Option<&agent::volumes::PinnedRevision> {
    record.volume_revisions.values().flatten().next()
}

fn optional_mounting_agent(name: &str, volume: &str) -> ApplyRequest {
    let mut request = mounting_agent(name, volume);
    if let MountSpec::Volume { optional, .. } = &mut request.agent.spec.sandbox.mounts[0] {
        *optional = true;
    }
    request
}

/// An optional volume that does not exist is left out rather than waited for, and stays out of the
/// agent's sandbox once it exists. One taking its first copy is waited for, as it is ready soon.
#[tokio::test(flavor = "local")]
async fn an_agent_leaves_out_an_optional_volume_that_does_not_exist_until_it_is_recreated() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-optional");
    let source = temporary.path().join("apps");
    std::fs::create_dir_all(&source).expect("source");
    let mounts = |record: &agent::control_plane::AgentRecord| {
        let fixture = &fixture;
        let name = record.sandbox_name().expect("sandbox name");
        async move { fixture.backend.find(&name).await.expect("sandbox").mounts }
    };
    fixture
        .control_plane
        .apply(optional_mounting_agent("reader", "apps"))
        .await
        .expect("apply agent");

    let reader = reconcile_agent(&fixture, "reader").await;
    assert_eq!(ready_reason(&reader).0, ConditionStatus::True);
    assert_eq!(
        reader.volume_revisions.values().collect::<Vec<_>>(),
        [&None],
        "the mount is left out for good"
    );
    assert_eq!(mounts(&reader).await, []);
    let optional = reader
        .agent
        .status
        .conditions
        .iter()
        .find(|condition| condition.kind == Condition::OPTIONAL_SOURCES)
        .expect("OptionalSources condition");
    assert_eq!(
        (optional.status, optional.message.as_str()),
        (
            ConditionStatus::False,
            "left out spec.sandbox.mounts[0].volume \"apps\", until the agent is recreated"
        )
    );

    fixture
        .service
        .apply(volume_request("apps", source))
        .await
        .expect("apply volume");
    fixture
        .control_plane
        .apply(optional_mounting_agent("second", "apps"))
        .await
        .expect("apply second agent");
    let second = reconcile_agent(&fixture, "second").await;
    assert_eq!(ready_reason(&second).2, "volume \"apps\" is taking its first copy");
    reconcile_volume(&fixture, "apps").await;
    let second = reconcile_agent(&fixture, "second").await;
    assert_eq!(mounts(&second).await.len(), 1);
    let reader = reconcile_agent(&fixture, "reader").await;
    assert_eq!(mounts(&reader).await, [], "an existing sandbox keeps its mounts");

    // Clients read each agent's choice from its status.
    for (name, revision) in [("reader", None), ("second", Some(1))] {
        let agent = fixture.control_plane.get(name).await.expect("agent");
        assert_eq!(
            agent.status.volumes,
            [agent::MountedVolume {
                target: SandboxPath::new("/home/agent/code/apps"),
                volume: "apps".into(),
                revision,
            }],
            "{name}"
        );
    }
}

/// A copy never changes, so a writable mount mounts it read-only too, and agent setup puts the
/// writable layer over it.
#[tokio::test(flavor = "local")]
async fn a_writable_volume_mount_mounts_its_copy_read_only() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-writable");
    let source = temporary.path().join("apps");
    std::fs::create_dir_all(&source).expect("source");
    fixture
        .service
        .apply(volume_request("apps", source))
        .await
        .expect("apply volume");
    reconcile_volume(&fixture, "apps").await;
    let mut request = mounting_agent("builder", "apps");
    if let MountSpec::Volume { read_only, .. } = &mut request.agent.spec.sandbox.mounts[0] {
        *read_only = false;
    }
    let resources = &mut request.agent.spec.sandbox.resources;
    resources.root_filesystem = sandbox::RootFilesystem::direct(resources.root_filesystem.capacity());
    fixture.control_plane.apply(request).await.expect("apply agent");

    let builder = reconcile_agent(&fixture, "builder").await;

    let sandbox = fixture
        .backend
        .find(&builder.sandbox_name().expect("sandbox name"))
        .await
        .expect("sandbox");
    assert_eq!(
        sandbox.mounts,
        [Mount::Volume {
            id: pinned(&builder).expect("pinned revision").storage.volume.clone(),
            target: SandboxPath::new("/home/agent/code/apps"),
            read_only: true,
        }]
    );
}

#[tokio::test(flavor = "local")]
async fn an_agent_missing_several_volumes_names_them_in_one_hint() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-missing");
    let source = temporary.path().join("docs");
    std::fs::create_dir_all(&source).expect("source");
    fixture
        .service
        .apply(volume_request("docs", source))
        .await
        .expect("apply volume");
    let mut request = mounting_agent("reader", "apps");
    for (volume, target) in [("docs", "/home/agent/code/docs"), ("notes", "/home/agent/code/notes")] {
        request.agent.spec.sandbox.mounts.push(MountSpec::Volume {
            volume: volume.into(),
            target: SandboxPath::new(target),
            read_only: true,
            optional: false,
        });
    }
    fixture.control_plane.apply(request).await.expect("apply agent");

    let waiting = reconcile_agent(&fixture, "reader").await;

    assert_eq!(
        ready_reason(&waiting).2,
        "volumes \"apps\", \"notes\" do not exist; apply manifests that declare them; volume \"docs\" is taking \
         its first copy"
    );
}

#[tokio::test(flavor = "local")]
async fn an_agent_waits_for_its_volume_and_keeps_the_revision_it_gets_first() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-agents");
    let source = temporary.path().join("apps");
    std::fs::create_dir_all(&source).expect("source");
    std::fs::write(source.join("one.txt"), b"one").expect("file");

    fixture
        .control_plane
        .apply(mounting_agent("first", "apps"))
        .await
        .expect("apply agent");
    let waiting = reconcile_agent(&fixture, "first").await;
    assert_eq!(
        ready_reason(&waiting),
        (
            ConditionStatus::False,
            Condition::REASON_WAITING_FOR_VOLUMES.into(),
            "volume \"apps\" does not exist; apply a manifest that declares it".into()
        )
    );
    assert_eq!(fixture.backend.count(), 0, "no sandbox is created without the volume");

    fixture
        .service
        .apply(volume_request("apps", source.clone()))
        .await
        .expect("apply volume");
    let taking = reconcile_agent(&fixture, "first").await;
    assert_eq!(ready_reason(&taking).2, "volume \"apps\" is taking its first copy");
    reconcile_volume(&fixture, "apps").await;
    let first = reconcile_agent(&fixture, "first").await;
    assert_eq!(ready_reason(&first).0, ConditionStatus::True);
    let first_revision = pinned(&first).expect("pinned revision").clone();
    assert_eq!(first_revision.revision, 1);
    let sandbox = fixture
        .backend
        .find(&first.sandbox_name().expect("sandbox name"))
        .await
        .expect("sandbox");
    assert_eq!(
        sandbox.mounts,
        [Mount::Volume {
            id: first_revision.storage.volume.clone(),
            target: SandboxPath::new("/home/agent/code/apps"),
            read_only: true,
        }]
    );

    std::fs::write(source.join("two.txt"), b"two").expect("new file");
    fixture
        .service
        .apply(volume_request("apps", source.clone()))
        .await
        .expect("apply changed volume");
    reconcile_volume(&fixture, "apps").await;
    fixture
        .control_plane
        .apply(mounting_agent("second", "apps"))
        .await
        .expect("apply second agent");
    let second = reconcile_agent(&fixture, "second").await;
    assert_eq!(pinned(&second).map(|pin| pin.revision), Some(2));
    assert_eq!(
        pinned(&reconcile_agent(&fixture, "first").await),
        Some(&first_revision),
        "an agent keeps the revision it got first"
    );
    assert_eq!(
        revision_agents(&fixture.service.get("apps").await.expect("volume")),
        [(1, vec!["first".to_owned()]), (2, vec!["second".to_owned()])],
        "a revision an agent uses stays"
    );

    fixture.control_plane.delete("first").await.expect("delete first agent");
    fixture
        .agent_reconciler
        .reconcile(first.id)
        .await
        .expect("release first agent");
    reconcile_volume(&fixture, "apps").await;
    assert_eq!(
        revision_agents(&fixture.service.get("apps").await.expect("volume")),
        [(1, Vec::new()), (2, vec!["second".to_owned()])],
        "the sandbox the agent's retention policy kept still mounts its revision"
    );

    fixture.backend.delete(&sandbox.id).await.expect("delete kept sandbox");
    reconcile_volume(&fixture, "apps").await;
    assert_eq!(
        revision_agents(&fixture.service.get("apps").await.expect("volume")),
        [(2, vec!["second".to_owned()])],
        "the revision goes once nothing uses it"
    );
    assert_eq!(fixture.backend.volume_count(), 1);
}

#[tokio::test(flavor = "local")]
async fn an_agent_whose_volume_has_no_copy_fails_until_the_volume_is_applied_again() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-failed");
    let source = temporary.path().join("apps");
    std::fs::create_dir_all(&source).expect("source");
    let changing = source.clone();
    *fixture.provider.while_copying.borrow_mut() = Some(Box::new(move || {
        std::fs::write(changing.join("late.txt"), b"late").expect("change while copying");
    }));
    fixture
        .service
        .apply(volume_request("apps", source))
        .await
        .expect("apply volume");
    reconcile_volume(&fixture, "apps").await;
    fixture
        .control_plane
        .apply(mounting_agent("worker", "apps"))
        .await
        .expect("apply agent");
    let failed = reconcile_agent(&fixture, "worker").await;
    assert!(
        ready_reason(&failed).2.contains("has no copy"),
        "{:?}",
        ready_reason(&failed)
    );
    assert_eq!(failed.agent.status.failure, Some(agent::FailureKind::Invalid));
}

/// A revision keeps the source it copied after the volume's source changes, and agents may still
/// mount it.
#[tokio::test(flavor = "local")]
async fn an_agent_cannot_select_a_secret_file_inside_an_earlier_source_of_a_volume() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-earlier-secret-file");
    let earlier = temporary.path().join("earlier");
    let later = temporary.path().join("later");
    std::fs::create_dir_all(&earlier).expect("earlier source");
    std::fs::create_dir_all(&later).expect("later source");
    std::fs::write(earlier.join("credentials.env"), b"GITHUB_TOKEN=value\n").expect("secret file");
    fixture
        .service
        .apply(volume_request("apps", earlier.clone()))
        .await
        .expect("apply");
    reconcile_volume(&fixture, "apps").await;
    fixture
        .service
        .apply(volume_request("apps", later))
        .await
        .expect("apply another source");

    let mut request = agent("worker");
    request.spec.secrets.push(agent::SecretSpec {
        environment: "GITHUB_TOKEN".into(),
        optional: false,
        placeholder: None,
        allowed_hosts: vec!["github.com".into()],
        source: None,
        secret: None,
    });
    let error = fixture
        .control_plane
        .apply(ApplyRequest {
            source_directory: std::env::temp_dir().join("agent-platform-source"),
            manifest_path: None,
            env_file: Some(earlier.join("credentials.env")),
            binds: Vec::new(),
            create_only: false,
            agent: request,
        })
        .await
        .expect_err("revision 1 holds the real values");
    assert!(error.to_string().contains("Volume \"apps\" copies"), "{error}");
}

#[tokio::test(flavor = "local")]
async fn an_agent_cannot_select_a_secret_file_inside_a_copied_directory() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-secret-file");
    let source = temporary.path().join("apps");
    std::fs::create_dir_all(&source).expect("source");
    std::fs::write(source.join("credentials.env"), b"GITHUB_TOKEN=value\n").expect("secret file");
    fixture
        .service
        .apply(volume_request("apps", source.clone()))
        .await
        .expect("a file that no agent selects yet is copied");

    let mut request = mounting_agent("worker", "apps");
    request.agent.spec.secrets.push(agent::SecretSpec {
        environment: "GITHUB_TOKEN".into(),
        optional: false,
        placeholder: None,
        allowed_hosts: vec!["github.com".into()],
        source: None,
        secret: None,
    });
    request.env_file = Some(source.join("credentials.env"));
    let error = fixture
        .control_plane
        .apply(request)
        .await
        .expect_err("a copy any agent may mount would hold the real values");
    assert!(error.to_string().contains("Volume \"apps\" copies"), "{error}");
}

#[tokio::test(flavor = "local")]
async fn a_copy_a_failed_copy_could_not_delete_is_deleted_by_cleanup() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-leftover");
    let source = temporary.path().join("apps");
    std::fs::create_dir_all(&source).expect("source");
    let vanishing = source.clone();
    *fixture.provider.while_copying.borrow_mut() = Some(Box::new(move || {
        std::fs::remove_dir_all(&vanishing).expect("source removed while copying");
    }));
    // The copy step and the cleanup in the same pass both fail to delete the copy.
    fixture.provider.failing_volume_deletes.set(2);
    fixture
        .service
        .apply(volume_request("apps", source))
        .await
        .expect("apply volume");
    let id = fixture.volumes.get_by_name("apps").await.expect("volume").id;
    fixture
        .reconciler
        .reconcile(id)
        .await
        .expect_err("the copy could not be deleted");
    assert_eq!(
        copies(&fixture.service.get("apps").await.expect("volume")),
        (vec![], Some((1, true)))
    );
    assert_eq!(fixture.backend.volume_count(), 1);
    assert_eq!(
        fixture.volumes.get(id).await.expect("volume").discarded.len(),
        1,
        "the volume still tracks the copy"
    );

    reconcile_volume(&fixture, "apps").await;
    assert_eq!(
        fixture.backend.volume_count(),
        0,
        "cleanup deletes the copy the failure left"
    );
    assert_eq!(
        copies(&fixture.service.get("apps").await.expect("volume")),
        (vec![], Some((1, true))),
        "the failure stays visible"
    );
}

/// A retry takes its copy under the same name as the one that failed, so a failed copy still there
/// is replaced rather than taken as the new one.
#[tokio::test(flavor = "local")]
async fn a_retried_copy_replaces_the_failed_copy_left_under_its_name() {
    let fixture = volume_fixture();
    let temporary = TempDirectory::new("volume-retry-leftover");
    let source = temporary.path().join("apps");
    std::fs::create_dir_all(&source).expect("source");
    let vanishing = source.clone();
    *fixture.provider.while_copying.borrow_mut() = Some(Box::new(move || {
        std::fs::remove_dir_all(&vanishing).expect("source removed while copying");
    }));
    fixture.provider.failing_volume_deletes.set(2);
    fixture
        .service
        .apply(volume_request("apps", source.clone()))
        .await
        .expect("apply volume");
    let id = fixture.volumes.get_by_name("apps").await.expect("volume").id;
    let _ = fixture.reconciler.reconcile(id).await;
    let failed = fixture.volumes.get(id).await.expect("volume").discarded[0]
        .volume
        .clone();

    *fixture.provider.while_copying.borrow_mut() = None;
    std::fs::create_dir_all(&source).expect("source restored");
    fixture
        .service
        .apply(volume_request("apps", source))
        .await
        .expect("apply volume again");
    reconcile_volume(&fixture, "apps").await;
    let record = fixture.volumes.get(id).await.expect("volume");
    let latest = record.volume.status.latest().expect("revision 1");
    assert_eq!(latest.number, 1, "the retry reuses the number");
    assert_ne!(
        latest.storage.volume, failed,
        "the failed copy is not taken as the new one"
    );
    assert_eq!(fixture.backend.volume_count(), 1);
    assert_eq!(record.discarded, []);
}
