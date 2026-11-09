//! Agent resource persistence with one column group per write owner.

use rusqlite::{Connection, OptionalExtension as _, params};

use crate::{Agent, AgentId, Error, Status, control_plane::AgentRecord};

use super::{database_error, secrets};

pub(super) fn get(connection: &Connection, id: AgentId) -> Result<AgentRecord, Error> {
    connection
        .query_row(
            "SELECT id, active_name, source_directory, desired_json, deletion_timestamp, status_json,
                    optional_mount_sources_json, volume_revisions_json, origin_json
             FROM agents WHERE id = ?1 AND active_name IS NOT NULL",
            [id.to_string()],
            decode_row,
        )
        .optional()
        .map_err(database_error)?
        .ok_or(Error::NotFound)
}

pub(super) fn get_by_name(connection: &Connection, name: &str) -> Result<AgentRecord, Error> {
    let record = connection
        .query_row(
            "SELECT id, active_name, source_directory, desired_json, deletion_timestamp, status_json,
                    optional_mount_sources_json, volume_revisions_json, origin_json
             FROM agents WHERE active_name = ?1",
            [name],
            decode_row,
        )
        .optional()
        .map_err(database_error)?
        .ok_or(Error::NotFound)?;
    if record.agent.metadata.name == name {
        Ok(record)
    } else {
        Err(Error::Database(
            "stored agent name does not match its active-name index".into(),
        ))
    }
}

pub(super) fn list(connection: &Connection) -> Result<Vec<AgentRecord>, Error> {
    let mut statement = connection
        .prepare(
            "SELECT id, active_name, source_directory, desired_json, deletion_timestamp, status_json,
                    optional_mount_sources_json, volume_revisions_json, origin_json
             FROM agents WHERE active_name IS NOT NULL ORDER BY active_name",
        )
        .map_err(database_error)?;
    statement
        .query_map([], decode_row)
        .map_err(database_error)?
        .map(|row| row.map_err(database_error))
        .collect()
}

pub(super) fn put(connection: &mut Connection, record: &AgentRecord, expected_generation: u64) -> Result<(), Error> {
    let id = record.id;
    let name = &record.agent.metadata.name;
    let source = serde_json::to_string(&crate::Provenance {
        source_directory: record.source_directory.clone(),
        manifest_path: record.manifest_path.clone(),
        env_file: record.env_file.clone(),
    })?;
    let desired = encode_desired(&record.agent)?;
    let origin = serde_json::to_string(&StoredOrigin {
        origin: record.origin.clone(),
        lifetime: record.lifetime,
        bindings: record.bindings.clone(),
    })?;
    let transaction = connection.transaction().map_err(database_error)?;
    let changed = if expected_generation == 0 {
        transaction
            .execute(
                "INSERT OR IGNORE INTO agents
                 (id, active_name, source_directory, desired_json, deletion_timestamp, status_json,
                  optional_mount_sources_json, volume_revisions_json, origin_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id.to_string(),
                    name,
                    source,
                    desired,
                    deletion_timestamp(&record.agent),
                    encode_status(&record.agent.status)?,
                    serde_json::to_string(&record.optional_mount_sources)?,
                    serde_json::to_string(&record.volume_revisions)?,
                    origin
                ],
            )
            .map_err(database_error)?
    } else {
        let current = get(&transaction, id)?;
        if current.agent.metadata.generation != expected_generation
            || current.agent.metadata.name != *name
            || current.agent.metadata.deletion_timestamp.is_some()
        {
            return Err(Error::Conflict);
        }
        transaction
            .execute(
                "UPDATE agents SET source_directory = ?1, desired_json = ?2, origin_json = ?5
                 WHERE id = ?3 AND active_name = ?4 AND deletion_timestamp IS NULL",
                params![source, desired, id.to_string(), name, origin],
            )
            .map_err(database_error)?
    };
    if changed != 1 {
        return Err(Error::Conflict);
    }
    transaction.commit().map_err(database_error)
}

pub(super) fn update_status(
    connection: &mut Connection,
    id: AgentId,
    generation: u64,
    mut status: Status,
) -> Result<Status, Error> {
    let transaction = connection.transaction().map_err(database_error)?;
    let record = get(&transaction, id)?;
    if record.agent.metadata.generation != generation || record.agent.metadata.deletion_timestamp.is_some() {
        return Err(Error::Conflict);
    }
    status.scrub_projected();
    status.stamp_transitions(&record.agent.status, time::OffsetDateTime::now_utc());
    let changed = transaction
        .execute(
            "UPDATE agents SET status_json = ?1 WHERE id = ?2 AND active_name IS NOT NULL",
            params![serde_json::to_string(&status)?, id.to_string()],
        )
        .map_err(database_error)?;
    if changed != 1 {
        return Err(Error::Conflict);
    }
    transaction.commit().map_err(database_error)?;
    Ok(status)
}

pub(super) fn mark_deleting(connection: &mut Connection, name: &str) -> Result<AgentRecord, Error> {
    let transaction = connection.transaction().map_err(database_error)?;
    let mut record = get_by_name(&transaction, name)?;
    if record.agent.metadata.deletion_timestamp.is_none() {
        let timestamp = time::OffsetDateTime::now_utc();
        let changed = transaction
            .execute(
                "UPDATE agents SET deletion_timestamp = ?1 WHERE id = ?2 AND active_name = ?3",
                params![timestamp.unix_timestamp(), record.id.to_string(), name],
            )
            .map_err(database_error)?;
        if changed != 1 {
            return Err(Error::Conflict);
        }
        record.agent.metadata.deletion_timestamp = Some(timestamp);
    }
    transaction.commit().map_err(database_error)?;
    Ok(record)
}

pub(super) fn set_volume_revisions(
    connection: &Connection,
    id: AgentId,
    revisions: &crate::volumes::VolumePins,
) -> Result<(), Error> {
    let updated = connection
        .execute(
            "UPDATE agents SET volume_revisions_json = ?2 WHERE id = ?1",
            params![id.to_string(), serde_json::to_string(revisions)?],
        )
        .map_err(database_error)?;
    if updated == 0 {
        return Err(Error::NotFound);
    }
    Ok(())
}

pub(super) fn finalize_deletion(connection: &mut Connection, id: AgentId, generation: u64) -> Result<(), Error> {
    let transaction = connection.transaction().map_err(database_error)?;
    let record = get(&transaction, id)?;
    if record.agent.metadata.generation != generation || record.agent.metadata.deletion_timestamp.is_none() {
        return Err(Error::Conflict);
    }
    let changed = transaction
        .execute(
            "UPDATE agents SET active_name = NULL WHERE id = ?1 AND active_name IS NOT NULL",
            [id.to_string()],
        )
        .map_err(database_error)?;
    if changed != 1 {
        return Err(Error::Conflict);
    }
    secrets::delete_agent_secrets(&transaction, id)?;
    secrets::delete_secret(&transaction, &super::ssh_host_key_name(id))?;
    transaction.commit().map_err(database_error)
}

fn encode_desired(agent: &Agent) -> Result<String, Error> {
    let mut desired = agent.clone();
    desired.metadata.deletion_timestamp = None;
    desired.status = Status::default();
    serde_json::to_string(&desired).map_err(Error::from)
}

/// Serializes status for storage, scrubbing what is API-projected.
fn encode_status(status: &Status) -> Result<String, Error> {
    let mut status = status.clone();
    status.scrub_projected();
    serde_json::to_string(&status).map_err(Error::from)
}

/// Source-column payload: current writes store [`crate::Provenance`]; rows
/// written before the manifest path was recorded hold a bare directory string.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum StoredSource {
    Provenance(crate::Provenance),
    Directory(std::path::PathBuf),
}

/// Origin-column payload: where the agent's definition comes from and how agentd runs it, which
/// [`AgentRecord`] keeps as separate fields.
#[derive(serde::Deserialize, serde::Serialize)]
struct StoredOrigin {
    #[serde(flatten)]
    origin: crate::Origin,
    #[serde(default, skip_serializing_if = "is_until_deleted")]
    lifetime: crate::Lifetime,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    bindings: std::collections::BTreeMap<::sandbox::SandboxPath, std::path::PathBuf>,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_until_deleted(lifetime: &crate::Lifetime) -> bool {
    *lifetime == crate::Lifetime::UntilDeleted
}

fn deletion_timestamp(agent: &Agent) -> Option<i64> {
    agent
        .metadata
        .deletion_timestamp
        .map(time::OffsetDateTime::unix_timestamp)
}

fn decode_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRecord> {
    let id = row.get::<_, String>(0)?;
    let active_name = row.get::<_, String>(1)?;
    let source = row.get::<_, String>(2)?;
    let desired = row.get::<_, String>(3)?;
    let deletion = row.get::<_, Option<i64>>(4)?;
    let status = row.get::<_, String>(5)?;
    let optional_mount_sources = row.get::<_, String>(6)?;
    let volume_revisions = row.get::<_, String>(7)?;
    let origin = serde_json::from_str::<StoredOrigin>(&row.get::<_, String>(8)?).map_err(conversion_error)?;
    let id = id.parse::<AgentId>().map_err(conversion_error)?;
    let (source_directory, manifest_path, env_file) = match serde_json::from_str(&source).map_err(conversion_error)? {
        StoredSource::Provenance(provenance) => (
            provenance.source_directory,
            provenance.manifest_path,
            provenance.env_file,
        ),
        StoredSource::Directory(directory) => (directory, None, None),
    };
    let mut agent = serde_json::from_str::<Agent>(&desired).map_err(conversion_error)?;
    if agent.metadata.name != active_name {
        return Err(rusqlite::Error::InvalidQuery);
    }
    agent.metadata.deletion_timestamp = deletion
        .map(time::OffsetDateTime::from_unix_timestamp)
        .transpose()
        .map_err(conversion_error)?;
    agent.status = serde_json::from_str(&status).map_err(conversion_error)?;
    Ok(AgentRecord {
        id,
        source_directory,
        manifest_path,
        env_file,
        agent,
        optional_mount_sources: serde_json::from_str(&optional_mount_sources).map_err(conversion_error)?,
        volume_revisions: serde_json::from_str(&volume_revisions).map_err(conversion_error)?,
        origin: origin.origin,
        lifetime: origin.lifetime,
        bindings: origin.bindings,
    })
}

fn conversion_error(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}
