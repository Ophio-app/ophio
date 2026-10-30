// SPDX-License-Identifier: Apache-2.0
//! SQLite transactions provide atomic offsets and immutable command intents.
use crate::{Error, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::OpenOptions,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

#[derive(Clone)]
pub(crate) struct Store(Arc<Inner>);
struct Inner {
    connection: Mutex<Connection>,
    _lock: store::InstanceLock,
    // Dropped after the lock: its receivers resolve once the directory is free.
    released: watch::Sender<()>,
}
impl Store {
    pub async fn open(dir: PathBuf) -> Result<Self> {
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&dir).map_err(|_|Error::State)?;
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|_| Error::State)?;
            }
            let lock = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(dir.join("channels.lock")).map_err(|_|Error::State)?;
            let lock = store::InstanceLock::acquire(lock).map_err(|_|Error::StateLocked)?;
            let conn = Connection::open(dir.join("channels.sqlite3")).map_err(|_|Error::State)?;
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS entries (bucket TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY(bucket,key));").map_err(|_|Error::State)?;
            Ok(Self(Arc::new(Inner { connection: Mutex::new(conn),_lock:lock,released:watch::channel(()).0 })))
        }).await.map_err(|_|Error::State)?
    }
    /// Changes to closed once the last clone is gone, including one held by a
    /// blocking operation whose caller was cancelled, and the directory is free.
    pub fn released(&self) -> watch::Receiver<()> {
        self.0.released.subscribe()
    }
    async fn with<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let inner = self.0.clone();
        tokio::task::spawn_blocking(move || {
            f(&mut *inner.connection.lock().map_err(|_| Error::State)?)
        })
        .await
        .map_err(|_| Error::State)?
    }
    pub async fn get<T: DeserializeOwned + Send + 'static>(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<Option<T>> {
        let (bucket, key) = (bucket.to_owned(), key.to_owned());
        self.with(move |c| {
            let json: Option<String> = c
                .query_row(
                    "SELECT value FROM entries WHERE bucket=?1 AND key=?2",
                    params![bucket, key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|_| Error::State)?;
            json.map(|j| serde_json::from_str(&j).map_err(|_| Error::State))
                .transpose()
        })
        .await
    }
    pub async fn put<T: Serialize>(&self, bucket: &str, key: &str, value: &T) -> Result<()> {
        let (bucket, key, json) = (
            bucket.to_owned(),
            key.to_owned(),
            serde_json::to_string(value).map_err(|_| Error::State)?,
        );
        self.with(move |c| { c.execute("INSERT INTO entries VALUES (?1,?2,?3) ON CONFLICT(bucket,key) DO UPDATE SET value=excluded.value",params![bucket,key,json]).map_err(|_|Error::State)?; Ok(()) }).await
    }
    pub async fn delete(&self, bucket: &str, key: &str) -> Result<()> {
        let (bucket, key) = (bucket.to_owned(), key.to_owned());
        self.with(move |c| {
            c.execute(
                "DELETE FROM entries WHERE bucket=?1 AND key=?2",
                params![bucket, key],
            )
            .map_err(|_| Error::State)?;
            Ok(())
        })
        .await
    }
    pub async fn put_batch(
        &self,
        values: Vec<(&'static str, String, serde_json::Value)>,
    ) -> Result<()> {
        self.with(move |c| {
            let tx=c.transaction().map_err(|_|Error::State)?;
            for (bucket,key,value) in values {
                tx.execute("INSERT INTO entries VALUES (?1,?2,?3) ON CONFLICT(bucket,key) DO UPDATE SET value=excluded.value",params![bucket,key,value.to_string()]).map_err(|_|Error::State)?;
            }
            tx.commit().map_err(|_|Error::State)
        }).await
    }
    pub async fn list<T: DeserializeOwned + Send + 'static>(&self, bucket: &str) -> Result<Vec<T>> {
        Ok(self
            .entries(bucket)
            .await?
            .into_iter()
            .map(|(_, value)| value)
            .collect())
    }
    pub async fn entries<T: DeserializeOwned + Send + 'static>(
        &self,
        bucket: &str,
    ) -> Result<Vec<(String, T)>> {
        let bucket = bucket.to_owned();
        self.with(move |c| {
            let mut statement = c
                .prepare("SELECT key, value FROM entries WHERE bucket=?1 ORDER BY key")
                .map_err(|_| Error::State)?;
            statement
                .query_map([bucket], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(|_| Error::State)?
                .map(|row| {
                    let (key, value) = row.map_err(|_| Error::State)?;
                    Ok((key, serde_json::from_str(&value).map_err(|_| Error::State)?))
                })
                .collect()
        })
        .await
    }
}
