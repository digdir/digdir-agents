//! Docker endpoint discovery for Dockerfile builds. Bollard reads `DOCKER_HOST`,
//! but does not read the CLI's selected context (including Docker Desktop's
//! per-user socket). Discover again for each build, including reconciliation
//! retries after Docker starts or `docker context use` changes the selection.

use std::{
    env,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use bollard::Docker;
use sandbox::Error;
use serde::{Deserialize, de::DeserializeOwned};
use sha2::{Digest as _, Sha256};

use crate::{encoding::lower_hex, error};

#[cfg(unix)]
const DEFAULT_HOST: &str = "unix:///var/run/docker.sock";
#[cfg(windows)]
const DEFAULT_HOST: &str = "npipe:////./pipe/docker_engine";

pub(super) async fn connect() -> Result<Docker, Error> {
    let config_dir = env::var_os("DOCKER_CONFIG")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::home_dir().map(|home| home.join(".docker")));
    let host = endpoint(
        config_dir.as_deref(),
        env::var("DOCKER_HOST").ok().as_deref(),
        env::var("DOCKER_CONTEXT").ok().as_deref(),
    )
    .await?;
    Docker::connect_with_host(&host).map_err(|failure| {
        Error::Backend(format!(
            "connect to Docker Engine at {host}: {failure}; start Docker and check `docker context ls` or agentd's DOCKER_HOST"
        ))
    })
}

async fn endpoint(config_dir: Option<&Path>, host: Option<&str>, context: Option<&str>) -> Result<String, Error> {
    // Match Docker's host override before consulting the saved currentContext.
    if let Some(host) = host.filter(|value| !value.is_empty()) {
        return Ok(host.to_owned());
    }
    let configured_context = match (context.filter(|value| !value.is_empty()), config_dir) {
        (Some(context), _) => context.to_owned(),
        (None, Some(config_dir)) => read_json::<Config>(&config_dir.join("config.json"))
            .await?
            .map(|config| config.current_context)
            .unwrap_or_default(),
        (None, None) => String::new(),
    };
    if configured_context.is_empty() || configured_context == "default" {
        return Ok(DEFAULT_HOST.to_owned());
    }
    let config_dir =
        config_dir.ok_or_else(|| Error::Backend("cannot locate Docker context: set DOCKER_CONFIG".to_owned()))?;
    // Docker CLI's context store keys metadata by the SHA-256 of its name.
    let context_id = lower_hex(&Sha256::digest(configured_context.as_bytes()));
    let metadata_path = config_dir.join("contexts/meta").join(context_id).join("meta.json");
    let metadata = read_json::<Context>(&metadata_path).await?.ok_or_else(|| {
        Error::Backend(format!(
            "Docker context {configured_context:?} was not found; check `docker context ls`"
        ))
    })?;
    let host = metadata.endpoints.docker.host;
    // Remote context transports and TLS material need their own integration.
    // Never silently use a different engine or discard a context's TLS settings.
    if !host.starts_with("unix://") && !host.starts_with("npipe://") {
        return Err(Error::Backend(format!(
            "Docker context {configured_context:?} must use a local Unix socket or named pipe for Dockerfile builds"
        )));
    }
    Ok(host)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    #[serde(default)]
    current_context: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Context {
    endpoints: Endpoints,
}

#[derive(Deserialize)]
struct Endpoints {
    docker: Endpoint,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Endpoint {
    host: String,
}

async fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, Error> {
    let contents = match tokio::fs::read(path).await {
        Ok(contents) => contents,
        Err(failure) if failure.kind() == ErrorKind::NotFound => return Ok(None),
        Err(failure) => return Err(error::io("read Docker configuration", failure)),
    };
    // Docker accepts a blank config.json as default configuration. A selected
    // context still requires metadata, so returning None cannot change engines.
    if contents.iter().all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n')) {
        return Ok(None);
    }
    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(|failure| Error::Backend(format!("parse Docker configuration {}: {failure}", path.display())))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    async fn write_context(config: &Path, name: &str, host: &str) {
        use sha2::{Digest as _, Sha256};
        let id = crate::encoding::lower_hex(&Sha256::digest(name.as_bytes()));
        let directory = config.join("contexts/meta").join(id);
        tokio::fs::create_dir_all(&directory)
            .await
            .expect("create context directory");
        tokio::fs::write(
            directory.join("meta.json"),
            json!({"Name": name, "Endpoints": {"docker": {"Host": host}}}).to_string(),
        )
        .await
        .expect("write context metadata");
    }

    async fn select_context(config: &Path, name: &str) {
        tokio::fs::write(config.join("config.json"), json!({"currentContext": name}).to_string())
            .await
            .expect("write Docker config");
    }

    #[tokio::test]
    async fn selected_desktop_context_uses_its_per_user_socket() {
        let config = tempfile::tempdir().expect("temporary Docker config");
        let host = "unix:///Users/developer/.docker/run/docker.sock";
        write_context(config.path(), "desktop-linux", host).await;
        select_context(config.path(), "desktop-linux").await;
        assert_eq!(
            super::endpoint(Some(config.path()), None, None)
                .await
                .expect("resolve context"),
            host
        );
    }

    #[tokio::test]
    async fn environment_host_overrides_context_and_unreadable_config() {
        let config = tempfile::tempdir().expect("temporary Docker config");
        tokio::fs::write(config.path().join("config.json"), "invalid JSON")
            .await
            .expect("write invalid config");
        let host = "unix:///run/podman/podman.sock";
        assert_eq!(
            super::endpoint(Some(config.path()), Some(host), Some("missing"))
                .await
                .expect("host override"),
            host
        );
    }

    #[tokio::test]
    async fn environment_context_overrides_saved_context_and_empty_host() {
        let config = tempfile::tempdir().expect("temporary Docker config");
        select_context(config.path(), "missing").await;
        let host = "unix:///Users/developer/.colima/default/docker.sock";
        write_context(config.path(), "colima", host).await;
        assert_eq!(
            super::endpoint(Some(config.path()), Some(""), Some("colima"))
                .await
                .expect("context override"),
            host
        );
        assert_eq!(
            super::endpoint(Some(config.path()), None, Some("default"))
                .await
                .expect("default context"),
            super::DEFAULT_HOST
        );
    }

    #[tokio::test]
    async fn absent_or_empty_selection_uses_platform_default() {
        let config = tempfile::tempdir().expect("temporary Docker config");
        for directory in [None, Some(config.path())] {
            assert_eq!(
                super::endpoint(directory, None, None).await.expect("default endpoint"),
                super::DEFAULT_HOST
            );
        }
        select_context(config.path(), "").await;
        assert_eq!(
            super::endpoint(Some(config.path()), Some(""), Some(""))
                .await
                .expect("empty selection"),
            super::DEFAULT_HOST
        );
    }

    #[tokio::test]
    async fn blank_configuration_uses_platform_default() {
        let config = tempfile::tempdir().expect("temporary Docker config");
        for contents in ["", " ", "\t\r\n ", "{}"] {
            tokio::fs::write(config.path().join("config.json"), contents)
                .await
                .expect("write blank Docker config");
            assert_eq!(
                super::endpoint(Some(config.path()), None, None)
                    .await
                    .expect("blank config selects the default engine"),
                super::DEFAULT_HOST
            );
        }
    }

    #[tokio::test]
    async fn broken_contexts_do_not_fall_back_to_another_engine() {
        let config = tempfile::tempdir().expect("temporary Docker config");
        select_context(config.path(), "missing").await;
        assert!(
            super::endpoint(Some(config.path()), None, None)
                .await
                .expect_err("missing context")
                .to_string()
                .contains("missing")
        );
        for contents in ["invalid JSON", "{", "{\"currentContext\":", "\u{000b}"] {
            tokio::fs::write(config.path().join("config.json"), contents)
                .await
                .expect("write invalid config");
            assert!(
                super::endpoint(Some(config.path()), None, None)
                    .await
                    .expect_err("invalid config")
                    .to_string()
                    .contains("parse Docker configuration")
            );
        }
        write_context(config.path(), "remote", "tcp://remote:2376").await;
        assert!(
            super::endpoint(Some(config.path()), None, Some("remote"))
                .await
                .expect_err("remote context")
                .to_string()
                .contains("local Unix socket or named pipe")
        );
    }
}
