//! Host-owned secret and provider-account persistence.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension as _, params};

use crate::{AgentId, Error};

use super::{
    NamedSecret, NamedSecretWrite, ProviderAccountWrite, StoredSecret, agent_secret_name, agent_secret_prefix,
    database_error, named_secret_name,
};

pub(super) fn set_secret(connection: &Connection, name: &str, value: &[u8]) -> Result<(), Error> {
    connection
        .execute(
            "INSERT INTO secrets (name, value) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET value = excluded.value",
            params![name, value],
        )
        .map(|_| ())
        .map_err(database_error)
}

pub(super) fn resolve_secret(
    connection: &Connection,
    name: &str,
) -> Result<sandbox::secret_store::SecretMaterial, Error> {
    connection
        .query_row("SELECT value FROM secrets WHERE name = ?1", [name], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .optional()
        .map_err(database_error)?
        .map(sandbox::secret_store::SecretMaterial::new)
        .ok_or(Error::NotFound)
}

pub(super) fn replace_agent_secrets(
    connection: &mut Connection,
    id: AgentId,
    secrets: &[StoredSecret],
) -> Result<(), Error> {
    let transaction = connection.transaction().map_err(database_error)?;
    let mut desired = BTreeSet::new();
    for secret in secrets {
        let name = agent_secret_name(id, &secret.name);
        if !desired.insert(name.clone()) {
            return Err(Error::Invalid(format!("duplicate agent secret {:?}", secret.name)));
        }
        set_secret(&transaction, &name, &secret.value)?;
    }

    let prefix = agent_secret_prefix(id);
    let existing = {
        let mut statement = transaction
            .prepare("SELECT name FROM secrets WHERE substr(name, 1, length(?1)) = ?1")
            .map_err(database_error)?;
        statement
            .query_map([&prefix], |row| row.get::<_, String>(0))
            .map_err(database_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?
    };
    for name in existing {
        if !desired.contains(&name) {
            transaction
                .execute("DELETE FROM secrets WHERE name = ?1", [&name])
                .map_err(database_error)?;
        }
    }
    transaction.commit().map_err(database_error)
}

pub(super) fn delete_secret(connection: &Connection, name: &str) -> Result<(), Error> {
    connection
        .execute("DELETE FROM secrets WHERE name = ?1", [name])
        .map(|_| ())
        .map_err(database_error)
}

pub(super) fn delete_agent_secrets(connection: &Connection, id: AgentId) -> Result<(), Error> {
    let prefix = agent_secret_prefix(id);
    connection
        .execute("DELETE FROM secrets WHERE substr(name, 1, length(?1)) = ?1", [&prefix])
        .map(|_| ())
        .map_err(database_error)
}

pub(super) fn put_provider_account(connection: &mut Connection, account: &ProviderAccountWrite) -> Result<(), Error> {
    let transaction = connection.transaction().map_err(database_error)?;
    for credential in &account.credentials {
        set_secret(&transaction, &credential.name, &credential.value)?;
    }
    transaction
        .execute(
            "INSERT INTO provider_accounts (provider, metadata_json) VALUES (?1, ?2)
             ON CONFLICT(provider) DO UPDATE SET metadata_json = excluded.metadata_json",
            params![account.provider, account.metadata_json],
        )
        .map_err(database_error)?;
    transaction.commit().map_err(database_error)
}

pub(super) fn provider_account_exists(connection: &Connection, provider: &str) -> Result<bool, Error> {
    connection
        .query_row(
            "SELECT 1 FROM provider_accounts WHERE provider = ?1",
            [provider],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
        .map_err(database_error)
}

pub(super) fn provider_account_metadata(connection: &Connection, provider: &str) -> Result<Option<String>, Error> {
    connection
        .query_row(
            "SELECT metadata_json FROM provider_accounts WHERE provider = ?1",
            [provider],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)
}

/// Lists `(provider, metadata_json)` of every provider account named `<namespace>/…`, by name.
pub(super) fn provider_accounts(connection: &Connection, namespace: &str) -> Result<Vec<(String, String)>, Error> {
    let prefix = format!("{namespace}/");
    let mut statement = connection
        .prepare(
            "SELECT provider, metadata_json FROM provider_accounts
             WHERE substr(provider, 1, length(?1)) = ?1 ORDER BY provider",
        )
        .map_err(database_error)?;
    statement
        .query_map([&prefix], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(database_error)?
        .collect::<Result<_, _>>()
        .map_err(database_error)
}

/// Removes a provider account and its credentials together.
pub(super) fn delete_provider_account(
    connection: &mut Connection,
    provider: &str,
    credentials: &[String],
) -> Result<(), Error> {
    let transaction = connection.transaction().map_err(database_error)?;
    for credential in credentials {
        delete_secret(&transaction, credential)?;
    }
    transaction
        .execute("DELETE FROM provider_accounts WHERE provider = ?1", [provider])
        .map_err(database_error)?;
    transaction.commit().map_err(database_error)
}

const NAMED_SECRET_COLUMNS: &str = "substr(name, length(?1) + 1), allowed_hosts_json, created_at, updated_at";

/// Keeps a secret under a name, replacing its value and, when given, its hosts. A new secret needs
/// hosts, so it is not kept without them, and `None` says so.
pub(super) fn put_named_secret(
    connection: &mut Connection,
    secret: &NamedSecretWrite,
) -> Result<Option<NamedSecret>, Error> {
    let transaction = connection.transaction().map_err(database_error)?;
    if secret.allowed_hosts_json.is_none() && named_secret(&transaction, &secret.name)?.is_none() {
        return Ok(None);
    }
    transaction
        .execute(
            "INSERT INTO secrets (name, value, allowed_hosts_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(name) DO UPDATE SET
                 value = excluded.value,
                 allowed_hosts_json = coalesce(excluded.allowed_hosts_json, secrets.allowed_hosts_json),
                 updated_at = excluded.updated_at",
            params![
                named_secret_name(&secret.name),
                secret.value.as_slice(),
                secret.allowed_hosts_json,
                secret.at
            ],
        )
        .map_err(database_error)?;
    let kept = named_secret(&transaction, &secret.name)?;
    transaction.commit().map_err(database_error)?;
    Ok(kept)
}

/// Replaces the hosts of the secret kept under `name`, keeping its value, when there is one.
pub(super) fn set_named_secret_hosts(
    connection: &Connection,
    name: &str,
    allowed_hosts_json: &str,
    at: i64,
) -> Result<(), Error> {
    connection
        .execute(
            "UPDATE secrets SET allowed_hosts_json = ?2, updated_at = ?3 WHERE name = ?1",
            params![named_secret_name(name), allowed_hosts_json, at],
        )
        .map(|_| ())
        .map_err(database_error)
}

/// The secret kept under `name`, without its value.
pub(super) fn named_secret(connection: &Connection, name: &str) -> Result<Option<NamedSecret>, Error> {
    connection
        .query_row(
            &format!("SELECT {NAMED_SECRET_COLUMNS} FROM secrets WHERE name = ?1 || ?2"),
            params![named_secret_name(""), name],
            named_secret_row,
        )
        .optional()
        .map_err(database_error)
}

/// Every secret kept under a name, by name, without their values.
pub(super) fn named_secrets(connection: &Connection) -> Result<Vec<NamedSecret>, Error> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT {NAMED_SECRET_COLUMNS} FROM secrets WHERE substr(name, 1, length(?1)) = ?1 ORDER BY name"
        ))
        .map_err(database_error)?;
    statement
        .query_map([named_secret_name("")], named_secret_row)
        .map_err(database_error)?
        .collect::<Result<_, _>>()
        .map_err(database_error)
}

/// Removes the secret kept under `name`, returning it, or `None` when there was none.
pub(super) fn delete_named_secret(connection: &mut Connection, name: &str) -> Result<Option<NamedSecret>, Error> {
    let transaction = connection.transaction().map_err(database_error)?;
    let removed = named_secret(&transaction, name)?;
    if removed.is_some() {
        delete_secret(&transaction, &named_secret_name(name))?;
    }
    transaction.commit().map_err(database_error)?;
    Ok(removed)
}

fn named_secret_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NamedSecret> {
    Ok(NamedSecret {
        name: row.get(0)?,
        allowed_hosts_json: row.get(1)?,
        created_at: row.get(2)?,
        updated_at: row.get(3)?,
    })
}
