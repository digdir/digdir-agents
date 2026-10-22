#![cfg(unix)]
#![allow(clippy::expect_used, clippy::significant_drop_tightening)]

use std::{fmt::Write as _, path::Path, process::Command, time::Duration};

use sandbox::{
    Platform, RootFilesystemMode,
    image::{ImageSource, ResolveRequest},
    provider::SandboxProvider as _,
};
use sandbox_microsandbox::MicrosandboxProvider;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::UnixListener,
    runtime::LocalRuntime,
};

#[test]
fn dockerfile_resolution_recovers_when_context_socket_appears() {
    // Isolate Docker's environment from the other tests without mutating the
    // process environment while Tokio or other tests may be running.
    const CHILD: &str = "SANDBOX_DOCKER_CONTEXT_TEST_CHILD";
    if let Some(directory) = std::env::var_os(CHILD) {
        LocalRuntime::new()
            .expect("test runtime")
            .block_on(exercise_context(Path::new(&directory)));
        return;
    }
    let directory = tempfile::tempdir().expect("temporary Docker configuration");
    let output = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "dockerfile_resolution_recovers_when_context_socket_appears",
            "--nocapture",
        ])
        .env(CHILD, directory.path())
        .env("DOCKER_CONFIG", directory.path())
        .env_remove("DOCKER_HOST")
        .env_remove("DOCKER_CONTEXT")
        .output()
        .expect("run isolated context test");
    assert!(
        output.status.success(),
        "isolated test failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn exercise_context(directory: &Path) {
    // Open the same long-lived Provider before Docker's context or socket
    // exists, as when agentd starts before Docker Desktop.
    let provider = MicrosandboxProvider::open(directory.join("provider"))
        .await
        .expect("open Provider before Docker starts");
    let socket_path = directory.join("desktop.sock");
    select_context(directory, "desktop-linux", &socket_path).await;
    let request = ResolveRequest {
        source: ImageSource::Build {
            context: directory.to_path_buf(),
            dockerfile: "missing.Dockerfile".into(),
            target: None,
        },
        platform: Platform::native("linux"),
        root_filesystem_mode: RootFilesystemMode::Direct,
    };
    let first = provider
        .image_backend()
        .resolve(&request)
        .await
        .expect_err("Docker has not started");
    assert!(
        first.to_string().contains(&socket_path.display().to_string()),
        "{first}"
    );

    let listener = UnixListener::bind(&socket_path).expect("start Docker on the per-user socket");
    let ping = async {
        let (mut stream, _) = listener.accept().await.expect("accept Docker ping");
        let mut request = [0_u8; 4096];
        let count = stream.read(&mut request).await.expect("read ping request");
        assert!(String::from_utf8_lossy(&request[..count]).contains("/_ping"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
            .await
            .expect("reply to Docker ping");
    };
    let (second, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(provider.image_backend().resolve(&request), ping)
    })
    .await
    .expect("retry reaches the context's Docker engine");
    // After connecting, resolution advances to preparing the build context.
    // Deliberately omit the Dockerfile so no actual image build is needed.
    let second = second.expect_err("the test Dockerfile is deliberately absent");
    assert!(second.to_string().contains("resolve Dockerfile"), "{second}");

    let other_socket = directory.join("colima.sock");
    select_context(directory, "colima", &other_socket).await;
    let third = provider
        .image_backend()
        .resolve(&request)
        .await
        .expect_err("Colima has not started");
    assert!(
        third.to_string().contains(&other_socket.display().to_string()),
        "{third}"
    );
}

async fn select_context(directory: &Path, name: &str, socket: &Path) {
    let mut id = String::with_capacity(64);
    for byte in Sha256::digest(name.as_bytes()) {
        write!(id, "{byte:02x}").expect("format context ID");
    }
    let metadata = directory.join("contexts/meta").join(id);
    tokio::fs::create_dir_all(&metadata)
        .await
        .expect("create context metadata directory");
    tokio::fs::write(
        metadata.join("meta.json"),
        json!({"Name": name, "Endpoints": {"docker": {"Host": format!("unix://{}", socket.display())}}}).to_string(),
    )
    .await
    .expect("write local context");
    tokio::fs::write(
        directory.join("config.json"),
        json!({"currentContext": name}).to_string(),
    )
    .await
    .expect("select context");
}
