//! The image a standard agent runs on. It is published, so the host needs no Docker. Before it is,
//! or where it cannot be pulled, setup builds it here from the standard agent's Dockerfile instead.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use agent::Error;
use oci_client::{
    Client, Reference,
    client::{ClientConfig, ClientProtocol},
    errors::OciDistributionError,
    secrets::RegistryAuth,
};
use sandbox::image::ImageSource;

/// The folder in the agent's that holds its image's sources when the image is built here.
pub(super) const SOURCES: &str = "image";

/// The file an image is built from, beside the files it copies.
pub(super) const DOCKERFILE: &str = "Dockerfile";

/// How long the registry gets to say whether it has an image.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Whether an image can be pulled.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum Pull {
    Available,
    /// Its registry answered that it has no such image anyone may pull.
    Missing,
    /// Its registry could not say, as when it is out of reach, for this reason.
    Unknown(String),
}

/// Asks `reference`'s registry whether it would let anyone pull it, as agentd pulls a public image.
pub(super) async fn check(reference: &str) -> Pull {
    let image: Reference = match reference.parse() {
        Ok(image) => image,
        Err(error) => return Pull::Unknown(format!("{reference} is not an image reference: {error}")),
    };
    let client = Client::new(ClientConfig {
        protocol: ClientProtocol::Https,
        read_timeout: Some(TIMEOUT),
        connect_timeout: Some(TIMEOUT),
        ..ClientConfig::default()
    });
    match client.fetch_manifest_digest(&image, &RegistryAuth::Anonymous).await {
        Ok(_) => Pull::Available,
        Err(error) => classify(&error),
    }
}

/// Whether a registry's refusal says the image is not there for anyone, or only that it could not
/// be asked.
fn classify(error: &OciDistributionError) -> Pull {
    match error {
        OciDistributionError::ImageManifestNotFoundError(_)
        | OciDistributionError::UnauthorizedError { .. }
        | OciDistributionError::AuthenticationFailure(_)
        | OciDistributionError::RegistryError { .. }
        | OciDistributionError::ServerError {
            code: 401 | 403 | 404, ..
        } => Pull::Missing,
        other => Pull::Unknown(other.to_string()),
    }
}

/// Whether `directory` holds a Dockerfile to build an image from.
pub(super) fn buildable(directory: &Path) -> bool {
    directory.join(DOCKERFILE).is_file()
}

/// An image built from the Dockerfile in `context`.
pub(super) fn built_from(context: PathBuf) -> ImageSource {
    ImageSource::Build {
        context,
        dockerfile: DOCKERFILE.into(),
        target: None,
    }
}

/// Copies the files beside `source`'s Dockerfile into `destination`, but not its manifests or the
/// person's values. The build applies the `.dockerignore` among them, and folders below the
/// Dockerfile are what the agent reads otherwise.
pub(super) fn copy_sources(source: &Path, destination: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let skipped =
            agent::manifest::is_manifest_filename(Path::new(&name)) || name.to_string_lossy().starts_with(".env");
        if entry.path().is_file() && !skipped {
            std::fs::copy(entry.path(), destination.join(&name))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use oci_client::errors::OciDistributionError;

    use super::{Pull, buildable, classify, copy_sources};

    #[test]
    fn a_registry_that_has_no_image_for_anyone_is_told_apart_from_one_out_of_reach() {
        let not_found = OciDistributionError::ServerError {
            code: 404,
            url: "https://ghcr.io/v2/x/manifests/latest".into(),
            message: "not found".into(),
        };
        assert_eq!(classify(&not_found), Pull::Missing);
        let unauthorized = OciDistributionError::UnauthorizedError {
            url: "https://ghcr.io".into(),
        };
        assert_eq!(classify(&unauthorized), Pull::Missing);
        let failing = OciDistributionError::ServerError {
            code: 503,
            url: "https://ghcr.io".into(),
            message: "unavailable".into(),
        };
        assert!(matches!(classify(&failing), Pull::Unknown(_)));
    }

    #[test]
    fn the_build_gets_the_files_beside_the_dockerfile_but_not_the_manifest_or_values() {
        let source = tempfile::tempdir().expect("a source");
        for name in ["Dockerfile", ".dockerignore", "tools.json", "agent.yaml", ".env"] {
            std::fs::write(source.path().join(name), name).expect("a file");
        }
        std::fs::create_dir(source.path().join("home")).expect("a folder");
        let destination = tempfile::tempdir().expect("a destination");
        copy_sources(source.path(), destination.path()).expect("the sources should copy");
        let mut copied: Vec<_> = std::fs::read_dir(destination.path())
            .expect("the copy")
            .map(|entry| entry.expect("an entry").file_name().to_string_lossy().into_owned())
            .collect();
        copied.sort();
        assert_eq!(copied, [".dockerignore", "Dockerfile", "tools.json"]);
        assert!(buildable(source.path()));
        assert!(!buildable(&source.path().join("home")));
    }
}
