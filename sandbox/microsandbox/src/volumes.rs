use std::path::PathBuf;

use sandbox::{Error, SandboxProgress, mount::Mount, volume};

use crate::{backend::MicrosandboxProvider, error, state::VolumeRecord};

impl MicrosandboxProvider {
    pub(crate) async fn ensure_volume_record(
        &self,
        request: volume::EnsureVolumeRequest,
        progress: SandboxProgress,
    ) -> Result<volume::Volume, Error> {
        let (id, name, contents) = request.into_parts();
        let record = match self.state.volume_by_name(&name).await {
            Ok(record) if record.contents != contents => return Err(Error::Immutable("contents")),
            Ok(record) => record,
            Err(error) if error.is_not_found() => {
                let record = VolumeRecord::new(id, name, contents);
                // A copy is built before its record is saved, so a record always has its image.
                // An image left by an interrupted build has no record, and the Provider that
                // removes unclaimed copies removes it when it next opens.
                if let volume::VolumeContents::DirectoryCopy { source, owner } = &record.contents {
                    let image = self.volume_copy_path(&record);
                    tokio::fs::create_dir_all(&self.volume_copies)
                        .await
                        .map_err(|source| error::io("create Volume copies", source))?;
                    crate::directory_image::build(source.clone(), *owner, image.clone(), &progress).await?;
                    if let Err(error) = self.state.save_volume(&record).await {
                        let _ = tokio::fs::remove_file(&image).await;
                        return Err(error);
                    }
                } else {
                    self.state.save_volume(&record).await?;
                }
                record
            }
            Err(error) => return Err(error),
        };
        self.ensure_volume_runtime(&record).await?;
        Ok(record.to_volume())
    }

    pub(crate) async fn delete_volume_record(&self, id: &volume::VolumeId) -> Result<(), Error> {
        let record = self.state.volume_by_id(id).await?;
        let mounted = self.state.sandbox_records().await?.iter().any(|sandbox| {
            sandbox
                .mounts
                .iter()
                .any(|mount| matches!(mount, Mount::Volume { id: mounted, .. } if mounted == id))
        });
        if mounted {
            return Err(Error::VolumeInUse(id.clone()));
        }
        if record.contents.is_read_only() {
            match tokio::fs::remove_file(self.volume_copy_path(&record)).await {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error::io("remove Volume copy", error));
                }
                _ => {}
            }
        } else {
            match self.client.scope(microsandbox::Volume::get(&record.runtime_name)).await {
                Ok(handle) => handle.remove().await.map_err(error::microsandbox)?,
                Err(microsandbox::MicrosandboxError::VolumeNotFound(_)) => {}
                Err(failure) => return Err(error::microsandbox(failure)),
            }
        }
        self.state.remove_volume(&record).await
    }

    pub(crate) async fn ensure_volume_runtime(&self, record: &VolumeRecord) -> Result<(), Error> {
        if record.contents.is_read_only() {
            return if tokio::fs::try_exists(self.volume_copy_path(record))
                .await
                .map_err(|source| error::io("find Volume copy", source))?
            {
                Ok(())
            } else {
                Err(Error::Backend(format!(
                    "the copy held by Volume {} is missing",
                    record.name
                )))
            };
        }
        match self.client.scope(microsandbox::Volume::get(&record.runtime_name)).await {
            Ok(_) => return Ok(()),
            Err(microsandbox::MicrosandboxError::VolumeNotFound(_)) => {}
            Err(failure) => return Err(error::microsandbox(failure)),
        }
        self.client
            .scope(microsandbox::Volume::builder(&record.runtime_name).directory().create())
            .await
            .map(|_| ())
            .map_err(error::microsandbox)
    }

    /// The image file holding a Volume's directory copy.
    pub(crate) fn volume_copy_path(&self, record: &VolumeRecord) -> PathBuf {
        self.volume_copies.join(format!("{}.erofs", record.runtime_name))
    }

    /// Removes the files in the copies directory that no Volume record accounts for: images
    /// whose build or record write was interrupted, and the temporary files of a build.
    pub(crate) async fn remove_unclaimed_volume_copies(&self) -> Result<(), Error> {
        let mut entries = match tokio::fs::read_dir(&self.volume_copies).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error::io("list Volume copies", error)),
        };
        let claimed = self
            .state
            .volume_records()
            .await?
            .iter()
            .filter(|record| record.contents.is_read_only())
            .map(|record| self.volume_copy_path(record))
            .collect::<Vec<_>>();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|source| error::io("read Volume copies", source))?
        {
            let path = entry.path();
            if !claimed.contains(&path) {
                tokio::fs::remove_file(&path)
                    .await
                    .map_err(|source| error::io("remove unclaimed Volume copy", source))?;
            }
        }
        Ok(())
    }
}

impl VolumeRecord {
    pub(crate) fn to_volume(&self) -> volume::Volume {
        volume::Volume {
            id: self.id.clone(),
            name: self.name.clone(),
            contents: self.contents.clone(),
        }
    }
}
