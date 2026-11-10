//! Copies of the stored secrets that another thread keeps, so that it reads them without waiting
//! for the database.
//!
//! The database thread sends a copy every stored secret when it subscribes, and again after every
//! write to them, in the order of the writes. A change reaches the copy in the order it was made,
//! so a secret whose hosts are narrowed before its value is replaced never reaches a host it no
//! longer allows with the new value.

use std::collections::BTreeMap;

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

use crate::Error;

/// Every stored secret, by name, at one point in the order of the writes to them.
pub(crate) type SecretSnapshot = BTreeMap<String, MirroredSecret>;

/// What the database thread sends a copy, in the order of the writes to the secrets.
pub(crate) enum MirrorMessage {
    /// Every stored secret, as it is after the writes before it.
    Secrets(SecretSnapshot),
    /// Answered once the copy holds the secrets sent before it.
    Reached(oneshot::Sender<()>),
}

/// A secret's value, and the hosts a secret stored under a name allows.
pub(crate) struct MirroredSecret {
    pub(crate) value: Zeroizing<Vec<u8>>,
    /// The JSON list of host patterns of a secret stored under a name.
    pub(crate) allowed_hosts_json: Option<String>,
}

/// Sends the copy, if there is one, every stored secret as it is now. A copy that is gone is
/// forgotten.
pub(super) fn publish(connection: &Connection, copy: &mut Option<mpsc::UnboundedSender<MirrorMessage>>) {
    let Some(snapshots) = copy else {
        return;
    };
    let secrets = read(connection).unwrap_or_else(|error| {
        // Without the secrets as stored, the copy holds none, so it substitutes none.
        tracing::error!(%error, "could not read the secrets to copy");
        BTreeMap::new()
    });
    if snapshots.send(MirrorMessage::Secrets(secrets)).is_err() {
        *copy = None;
    }
}

/// Sends the copy, if there is one, `marker`, which it answers once it holds the secrets sent
/// before it. Without a copy, `marker` is dropped, as there is nothing to wait for.
pub(super) fn mark(copy: Option<&mpsc::UnboundedSender<MirrorMessage>>, marker: oneshot::Sender<()>) {
    if let Some(snapshots) = copy {
        let _ = snapshots.send(MirrorMessage::Reached(marker));
    }
}

fn read(connection: &Connection) -> Result<SecretSnapshot, Error> {
    let mut statement = connection
        .prepare_cached("SELECT name, value, allowed_hosts_json FROM secrets")
        .map_err(super::database_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                MirroredSecret {
                    value: Zeroizing::new(row.get(1)?),
                    allowed_hosts_json: row.get(2)?,
                },
            ))
        })
        .map_err(super::database_error)?;
    rows.collect::<Result<_, _>>().map_err(super::database_error)
}
