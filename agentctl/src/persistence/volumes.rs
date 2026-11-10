use rusqlite::{Connection, OptionalExtension as _, params};

use crate::{
    Error,
    volumes::{VolumeId, VolumeRecord},
};

use super::database_error;

pub(super) fn get(connection: &Connection, id: VolumeId) -> Result<VolumeRecord, Error> {
    connection
        .query_row(
            "SELECT record_json FROM volumes WHERE id = ?1",
            [id.to_string()],
            decode_row,
        )
        .optional()
        .map_err(database_error)?
        .ok_or_else(|| Error::VolumeNotFound(id.to_string()))
}

pub(super) fn get_by_name(connection: &Connection, name: &str) -> Result<VolumeRecord, Error> {
    connection
        .query_row("SELECT record_json FROM volumes WHERE name = ?1", [name], decode_row)
        .optional()
        .map_err(database_error)?
        .ok_or_else(|| Error::VolumeNotFound(name.to_owned()))
}

pub(super) fn list(connection: &Connection) -> Result<Vec<VolumeRecord>, Error> {
    let mut statement = connection
        .prepare("SELECT record_json FROM volumes ORDER BY name")
        .map_err(database_error)?;
    statement
        .query_map([], decode_row)
        .map_err(database_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)
}

pub(super) fn put(connection: &mut Connection, record: &VolumeRecord, expected_version: u64) -> Result<(), Error> {
    let transaction = connection.transaction().map_err(database_error)?;
    let stored = transaction
        .query_row(
            "SELECT version FROM volumes WHERE id = ?1",
            [record.id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(database_error)?;
    let expected = i64::try_from(expected_version).map_err(|_| Error::Conflict)?;
    if stored.unwrap_or_default() != expected {
        return Err(Error::Conflict);
    }
    let name = &record.volume.metadata.name;
    let taken = transaction
        .query_row(
            "SELECT 1 FROM volumes WHERE name = ?1 AND id != ?2",
            params![name, record.id.to_string()],
            |_| Ok(()),
        )
        .optional()
        .map_err(database_error)?
        .is_some();
    if taken {
        return Err(Error::Conflict);
    }
    let mut stored_record = record.clone();
    stored_record.version = expected_version + 1;
    let json = serde_json::to_string(&stored_record)?;
    transaction
        .execute(
            "INSERT INTO volumes (id, name, version, record_json) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET name = excluded.name, version = excluded.version,
                 record_json = excluded.record_json",
            params![record.id.to_string(), name, expected + 1, json],
        )
        .map_err(database_error)?;
    transaction.commit().map_err(database_error)
}

pub(super) fn remove(connection: &Connection, id: VolumeId, expected_version: u64) -> Result<(), Error> {
    let expected = i64::try_from(expected_version).map_err(|_| Error::Conflict)?;
    let removed = connection
        .execute(
            "DELETE FROM volumes WHERE id = ?1 AND version = ?2",
            params![id.to_string(), expected],
        )
        .map_err(database_error)?;
    if removed == 0 {
        return Err(Error::Conflict);
    }
    Ok(())
}

fn decode_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VolumeRecord> {
    let json = row.get::<_, String>(0)?;
    super::decode_json(&json)
        .map_err(|error| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error)))
}
