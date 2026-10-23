//! Docker endpoint discovery for Dockerfile builds. Bollard reads `DOCKER_HOST`,
//! but does not read the CLI's selected context (including Docker Desktop's
//! per-user socket). Discover again for each build, including reconciliation
//! retries after Docker starts or `docker context use` changes the selection.

use std::{
    env,
    ffi::OsString,
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
    let host = endpoint(&Selection::from_environment()).await?;
    Docker::connect_with_host(&host).map_err(|failure| {
        Error::Backend(format!(
            "connect to Docker Engine at {host}: {failure}; start Docker and check `docker context ls` or agentd's DOCKER_HOST"
        ))
    })
}

/// The Docker variables that select an engine.
#[derive(Default)]
struct Selection {
    config_dir: Option<PathBuf>,
    host: Option<String>,
    context: Option<String>,
}

impl Selection {
    fn from_environment() -> Self {
        Self::from_variables(env::var_os)
    }

    /// Like Docker, treats a variable set to an empty value as unset.
    fn from_variables(lookup: impl Fn(&'static str) -> Option<OsString>) -> Self {
        let variable = |name| lookup(name).filter(|value| !value.is_empty());
        let text = |name| variable(name).and_then(|value| value.into_string().ok());
        Self {
            config_dir: variable("DOCKER_CONFIG")
                .map(PathBuf::from)
                .or_else(|| env::home_dir().map(|home| home.join(".docker"))),
            host: text("DOCKER_HOST"),
            context: text("DOCKER_CONTEXT"),
        }
    }
}

async fn endpoint(selection: &Selection) -> Result<String, Error> {
    // Match Docker's host override before consulting the saved currentContext.
    if let Some(host) = &selection.host {
        return Ok(host.clone());
    }
    let config_dir = selection.config_dir.as_deref();
    let context = match &selection.context {
        Some(context) => Some(context.clone()),
        None => saved_context(config_dir).await?,
    };
    let Some(context) = context.filter(|name| name != "default") else {
        return Ok(DEFAULT_HOST.to_owned());
    };
    local_context_host(config_dir, &context).await
}

/// The context `docker context use` saved in `config.json`, if any.
async fn saved_context(config_dir: Option<&Path>) -> Result<Option<String>, Error> {
    let Some(config_dir) = config_dir else {
        return Ok(None);
    };
    let path = config_dir.join("config.json");
    let Some(contents) = read(&path, "read Docker configuration").await? else {
        return Ok(None);
    };
    // Docker reads a config.json holding only JSON whitespace as the default configuration.
    if contents.iter().all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n')) {
        return Ok(None);
    }
    let config: Config = parse(&path, &contents, "Docker configuration")?;
    Ok(config.current_context.filter(|name| !name.is_empty()))
}

async fn local_context_host(config_dir: Option<&Path>, context: &str) -> Result<String, Error> {
    let config_dir =
        config_dir.ok_or_else(|| Error::Backend("cannot locate Docker context: set DOCKER_CONFIG".to_owned()))?;
    // Docker CLI's context store keys metadata by the SHA-256 of its name.
    let context_id = lower_hex(&Sha256::digest(context.as_bytes()));
    let path = config_dir.join("contexts/meta").join(context_id).join("meta.json");
    let contents = read(&path, "read Docker context metadata").await?.ok_or_else(|| {
        Error::Backend(format!(
            "Docker context {context:?} was not found; check `docker context ls`"
        ))
    })?;
    let metadata: Context = parse(&path, &contents, "Docker context metadata")?;
    let host = metadata.endpoints.docker.host;
    // Remote context transports and TLS material need their own integration.
    // Never silently use a different engine or discard a context's TLS settings.
    if !host.starts_with("unix://") && !host.starts_with("npipe://") {
        return Err(Error::Backend(format!(
            "Docker context {context:?} must use a local Unix socket or named pipe for Dockerfile builds"
        )));
    }
    Ok(host)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    current_context: Option<String>,
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

async fn read(path: &Path, operation: &'static str) -> Result<Option<Vec<u8>>, Error> {
    match tokio::fs::read(path).await {
        Ok(contents) => Ok(Some(contents)),
        Err(failure) if failure.kind() == ErrorKind::NotFound => Ok(None),
        Err(failure) => Err(error::io(operation, failure)),
    }
}

fn parse<T: DeserializeOwned>(path: &Path, contents: &[u8], description: &str) -> Result<T, Error> {
    serde_json::from_slice(contents)
        .map_err(|failure| Error::Backend(format!("parse {description} {}: {failure}", path.display())))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::json;
    use sha2::{Digest as _, Sha256};

    use super::{DEFAULT_HOST, Selection, endpoint};

    /// Selects with `DOCKER_CONFIG` set to `config` and the other Docker variables from `variables`.
    fn selection(config: &Path, variables: &[(&str, &str)]) -> Selection {
        Selection::from_variables(|name| {
            if name == "DOCKER_CONFIG" {
                return Some(config.into());
            }
            variables
                .iter()
                .find(|(variable, _)| *variable == name)
                .map(|(_, value)| value.into())
        })
    }

    fn metadata_path(config: &Path, name: &str) -> PathBuf {
        let id = crate::encoding::lower_hex(&Sha256::digest(name.as_bytes()));
        config.join("contexts/meta").join(id).join("meta.json")
    }

    async fn write_context(config: &Path, name: &str, host: &str) {
        let path = metadata_path(config, name);
        tokio::fs::create_dir_all(path.parent().expect("context directory"))
            .await
            .expect("create context directory");
        tokio::fs::write(
            path,
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
            endpoint(&selection(config.path(), &[])).await.expect("resolve context"),
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
            endpoint(&selection(
                config.path(),
                &[("DOCKER_HOST", host), ("DOCKER_CONTEXT", "missing")]
            ))
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
            endpoint(&selection(
                config.path(),
                &[("DOCKER_HOST", ""), ("DOCKER_CONTEXT", "colima")]
            ))
            .await
            .expect("context override"),
            host
        );
        assert_eq!(
            endpoint(&selection(config.path(), &[("DOCKER_CONTEXT", "default")]))
                .await
                .expect("default context"),
            DEFAULT_HOST
        );
    }

    #[tokio::test]
    async fn absent_or_empty_selection_uses_platform_default() {
        let config = tempfile::tempdir().expect("temporary Docker config");
        for selection in [Selection::default(), selection(config.path(), &[])] {
            assert_eq!(endpoint(&selection).await.expect("default endpoint"), DEFAULT_HOST);
        }
        select_context(config.path(), "").await;
        assert_eq!(
            endpoint(&selection(
                config.path(),
                &[("DOCKER_HOST", ""), ("DOCKER_CONTEXT", "")]
            ))
            .await
            .expect("empty selection"),
            DEFAULT_HOST
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
                endpoint(&selection(config.path(), &[]))
                    .await
                    .expect("blank config selects the default engine"),
                DEFAULT_HOST
            );
        }
    }

    #[tokio::test]
    async fn broken_contexts_do_not_fall_back_to_another_engine() {
        let config = tempfile::tempdir().expect("temporary Docker config");
        select_context(config.path(), "missing").await;
        assert!(
            endpoint(&selection(config.path(), &[]))
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
                endpoint(&selection(config.path(), &[]))
                    .await
                    .expect_err("invalid config")
                    .to_string()
                    .contains("parse Docker configuration")
            );
        }
        write_context(config.path(), "blank", "unix:///run/docker.sock").await;
        tokio::fs::write(metadata_path(config.path(), "blank"), "")
            .await
            .expect("blank context metadata");
        assert!(
            endpoint(&selection(config.path(), &[("DOCKER_CONTEXT", "blank")]))
                .await
                .expect_err("blank context metadata")
                .to_string()
                .contains("parse Docker context metadata")
        );
        write_context(config.path(), "remote", "tcp://remote:2376").await;
        assert!(
            endpoint(&selection(config.path(), &[("DOCKER_CONTEXT", "remote")]))
                .await
                .expect_err("remote context")
                .to_string()
                .contains("local Unix socket or named pipe")
        );
    }
}
