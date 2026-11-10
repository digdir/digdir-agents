//! The stored secrets, copied to the thread that decides the sandboxes' requests, so that no
//! request waits for the database.

use std::{cell::RefCell, rc::Rc};

use sandbox::{
    LocalFuture,
    secret_store::{SecretMaterial, SecretReference, SecretStore},
};
use tokio::sync::mpsc;

use crate::{
    Error,
    persistence::{self, MirrorMessage, SecretSnapshot},
};

/// A copy of every stored secret, which the database keeps current: each write to the secrets
/// reaches it in the order of the writes.
pub struct SecretMirror {
    database: persistence::Database,
    secrets: RefCell<SecretSnapshot>,
}

impl SecretMirror {
    /// Copies every secret stored in `database`, and returns once the copy holds them. It is kept
    /// current on this thread from then on.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot send them.
    pub async fn subscribe(database: &persistence::Database) -> Result<Rc<Self>, Error> {
        let (snapshots, mut received) = mpsc::unbounded_channel();
        database.mirror_secrets(snapshots).await?;
        let Some(MirrorMessage::Secrets(first)) = received.recv().await else {
            return Err(Error::Daemon("the database did not send the secrets".into()));
        };
        let mirror = Rc::new(Self {
            database: database.clone(),
            secrets: RefCell::new(first),
        });
        let applying = Rc::downgrade(&mirror);
        tokio::task::spawn_local(async move {
            while let Some(message) = received.recv().await {
                let Some(mirror) = applying.upgrade() else {
                    return;
                };
                match message {
                    MirrorMessage::Secrets(snapshot) => *mirror.secrets.borrow_mut() = snapshot,
                    MirrorMessage::Reached(marker) => {
                        let _ = marker.send(());
                    }
                }
            }
        });
        Ok(mirror)
    }

    /// Waits until the copy holds every write to the secrets that returned before this call, such
    /// as those that prepared an agent whose policy is installed next.
    ///
    /// # Errors
    ///
    /// Returns an error when the database thread has stopped.
    pub(crate) async fn caught_up(&self) -> Result<(), Error> {
        self.database.mirror_caught_up().await
    }

    /// The hosts the secret stored as `name` allows, or `None` when it is not stored.
    pub(crate) fn allowed_hosts(&self, name: &str) -> Option<Vec<String>> {
        let secrets = self.secrets.borrow();
        let json = secrets
            .get(&persistence::named_secret_name(name))?
            .allowed_hosts_json
            .as_deref()?;
        serde_json::from_str(json).ok()
    }
}

impl SecretStore for SecretMirror {
    fn set<'a>(&'a self, _name: &'a str, _value: &'a [u8]) -> LocalFuture<'a, Result<SecretReference, sandbox::Error>> {
        Box::pin(async {
            Err(sandbox::Error::Backend(
                "a copy of the stored secrets cannot store one".into(),
            ))
        })
    }

    fn resolve<'a>(
        &'a self,
        reference: &'a SecretReference,
    ) -> LocalFuture<'a, Result<SecretMaterial, sandbox::Error>> {
        let value = self
            .secrets
            .borrow()
            .get(reference.as_str())
            .map(|secret| SecretMaterial::new(secret.value.to_vec()));
        Box::pin(
            async move { value.ok_or_else(|| sandbox::Error::not_found(sandbox::ResourceKind::Secret, "host secret")) },
        )
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::time::Duration;

    use sandbox::secret_store::{SecretReference, SecretStore as _};
    use tokio::sync::mpsc;
    use zeroize::Zeroizing;

    use super::SecretMirror;
    use crate::persistence::{self, Database, NamedSecretWrite};

    fn database(directory: &tempfile::TempDir) -> Database {
        Database::open(&directory.path().join("agent.db")).expect("database")
    }

    async fn store(database: &Database, name: &str, value: &[u8], host: &str) {
        database
            .put_named_secret(NamedSecretWrite {
                name: name.into(),
                value: Zeroizing::new(value.to_vec()),
                allowed_hosts_json: Some(serde_json::json!([host]).to_string()),
                at: 0,
            })
            .await
            .expect("store secret");
    }

    async fn value(mirror: &SecretMirror, name: &str) -> Option<Vec<u8>> {
        let reference = SecretReference::from_opaque(persistence::named_secret_name(name));
        mirror
            .resolve(&reference)
            .await
            .ok()
            .map(|material| material.expose().to_vec())
    }

    #[tokio::test(flavor = "local")]
    async fn the_copy_holds_the_secrets_once_it_subscribes_and_follows_their_writes() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let database = database(&directory);
        store(&database, "token", b"first", "api.example.com").await;
        let mirror = SecretMirror::subscribe(&database).await.expect("subscribe");
        assert_eq!(value(&mirror, "token").await.as_deref(), Some(&b"first"[..]));
        assert_eq!(mirror.allowed_hosts("token"), Some(vec!["api.example.com".to_owned()]));

        store(&database, "token", b"second", "other.example.com").await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while value(&mirror, "token").await.as_deref() != Some(&b"second"[..]) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the copy should follow the write");
        assert_eq!(
            mirror.allowed_hosts("token"),
            Some(vec!["other.example.com".to_owned()])
        );

        database.delete_named_secret("token").await.expect("delete secret");
        tokio::time::timeout(Duration::from_secs(5), async {
            while value(&mirror, "token").await.is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the copy should follow the deletion");
        assert_eq!(mirror.allowed_hosts("token"), None);
    }

    /// Narrowing a secret's hosts before replacing its value keeps the new value from every host
    /// the change takes away, as each write reaches the copy in its order.
    #[tokio::test(flavor = "local")]
    async fn writes_reach_the_copy_in_their_order() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let database = database(&directory);
        store(&database, "token", b"first", "api.example.com").await;
        let (snapshots, mut received) = mpsc::unbounded_channel();
        database.mirror_secrets(snapshots).await.expect("subscribe");
        let name = persistence::named_secret_name("token");
        let seen = |snapshot: &persistence::SecretSnapshot| {
            let secret = &snapshot[&name];
            (secret.value.to_vec(), secret.allowed_hosts_json.clone().expect("hosts"))
        };
        let mut next = async || match received.recv().await {
            Some(persistence::MirrorMessage::Secrets(snapshot)) => snapshot,
            _ => panic!("the database should send the secrets"),
        };
        let first = next().await;

        database
            .set_named_secret_hosts("token", serde_json::json!(["other.example.com"]).to_string(), 0)
            .await
            .expect("narrow hosts");
        store(&database, "token", b"second", "other.example.com").await;

        let narrowed = next().await;
        let replaced = next().await;
        assert_eq!(seen(&first), (b"first".to_vec(), r#"["api.example.com"]"#.to_owned()));
        assert_eq!(
            seen(&narrowed),
            (b"first".to_vec(), r#"["other.example.com"]"#.to_owned())
        );
        assert_eq!(
            seen(&replaced),
            (b"second".to_vec(), r#"["other.example.com"]"#.to_owned())
        );
    }

    /// A write that returned is in the copy once it has caught up, as an agent's own secret written
    /// as it is prepared must be before its policy allows the secret's new hosts.
    #[tokio::test(flavor = "local")]
    async fn a_copy_that_caught_up_holds_every_write_that_returned() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let database = database(&directory);
        store(&database, "token", b"first", "api.example.com").await;
        let mirror = SecretMirror::subscribe(&database).await.expect("subscribe");

        store(&database, "token", b"second", "other.example.com").await;
        mirror.caught_up().await.expect("catch up");
        assert_eq!(value(&mirror, "token").await.as_deref(), Some(&b"second"[..]));
        assert_eq!(
            mirror.allowed_hosts("token"),
            Some(vec!["other.example.com".to_owned()])
        );
    }
}
