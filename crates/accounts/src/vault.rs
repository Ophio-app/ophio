// SPDX-License-Identifier: Apache-2.0
//! Where keys, OAuth clients and refresh tokens are kept: the desktop's
//! keyring where there is one, otherwise a file only the owner can read.
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use zeroize::Zeroizing;

/// What is kept for an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Item {
    /// An API key the owner set.
    Key,
    /// An OAuth client's JSON.
    Client,
    /// An OAuth refresh token.
    Refresh,
    /// An agent login's password.
    Password,
    /// An agent login's authenticator setup key.
    CodeKey,
}
impl Item {
    pub fn name(self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::Client => "client",
            Self::Refresh => "refresh",
            Self::Password => "password",
            Self::CodeKey => "code_key",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct VaultError(pub String);

#[async_trait]
pub trait Vault: Send + Sync + std::fmt::Debug + 'static {
    /// As the Accounts screen names it, such as "Secret Service".
    fn name(&self) -> String;
    /// Why this is weaker than the keyring, if it is.
    fn note(&self) -> Option<String> {
        None
    }
    async fn get(&self, account: &str, item: Item)
    -> Result<Option<Zeroizing<String>>, VaultError>;
    async fn set(&self, account: &str, item: Item, value: &str) -> Result<(), VaultError>;
    /// Removing what isn't there is not an error.
    async fn remove(&self, account: &str, item: Item) -> Result<(), VaultError>;
    /// Removes everything Ophio keeps here, for every account.
    async fn clear(&self) -> Result<(), VaultError>;
}

/// The keyring when the desktop offers an unlocked one; otherwise a private
/// file under `dir`, with a note saying so.
pub async fn open(dir: &Path) -> std::sync::Arc<dyn Vault> {
    #[cfg(target_os = "linux")]
    match secret_service::SecretService::connect().await {
        Ok(service) => return with_keyring(dir, service),
        Err(e) => {
            tracing::info!("keyring unavailable, keeping keys in a private file: {e}");
            return without_keyring(dir);
        }
    }
    #[allow(unreachable_code)]
    std::sync::Arc::new(FileVault::new(dir.join("vault")))
}

/// Marks that Ophio has kept values in the keyring on this computer.
const KEYRING_USED: &str = "keyring-used";

/// `keyring`, with values left in the private file read and moved into it.
/// It is used only once noted as used, so a later start without it knows
/// what it can't reach; unnoted, the private file is used instead.
pub fn with_keyring<V: Vault>(dir: &Path, keyring: V) -> std::sync::Arc<dyn Vault> {
    let marker = dir.join(KEYRING_USED);
    let noted = match marker.try_exists() {
        Ok(true) => Ok(()),
        _ => {
            crate::files::private_dir(dir).and_then(|()| crate::files::write_private(&marker, b""))
        }
    };
    if let Err(e) = noted {
        tracing::warn!(
            "accounts: not using the keyring, as Ophio could not note that it does: {e}"
        );
        return file_vault(
            dir,
            "Ophio could not write in its accounts folder, so it is not using the keyring and keeps keys in a file only your user can read.",
        );
    }
    std::sync::Arc::new(WithLeftovers::new(
        keyring,
        FileVault::new(dir.join("vault")),
    ))
}

/// The private file, for a start that can't open the keyring.
pub fn without_keyring(dir: &Path) -> std::sync::Arc<dyn Vault> {
    file_vault(
        dir,
        "This computer has no unlocked keyring, so keys are kept in a file only your user can read.",
    )
}

/// The private file with `note`. If the keyring held Ophio's values before,
/// or that can't be told, nothing may be reported removed while they may
/// still be there.
fn file_vault(dir: &Path, note: &str) -> std::sync::Arc<dyn Vault> {
    let file = FileVault::new(dir.join("vault")).with_note(note);
    if dir.join(KEYRING_USED).try_exists().unwrap_or(true) {
        std::sync::Arc::new(KeyringAway { file })
    } else {
        std::sync::Arc::new(file)
    }
}

/// The private file while a keyring holding Ophio's values is locked or not
/// running: removing anything also needs the keyring, so it is refused once
/// the file's copy is gone.
#[derive(Debug)]
pub struct KeyringAway {
    pub file: FileVault,
}
impl KeyringAway {
    fn away() -> VaultError {
        VaultError(
            "The keyring is locked or not running, and Ophio has kept keys in it. Unlock it, then try again."
                .into(),
        )
    }
}
#[async_trait]
impl Vault for KeyringAway {
    fn name(&self) -> String {
        self.file.name()
    }
    fn note(&self) -> Option<String> {
        self.file.note()
    }
    async fn get(
        &self,
        account: &str,
        item: Item,
    ) -> Result<Option<Zeroizing<String>>, VaultError> {
        self.file.get(account, item).await
    }
    async fn set(&self, account: &str, item: Item, value: &str) -> Result<(), VaultError> {
        self.file.set(account, item, value).await
    }
    async fn remove(&self, account: &str, item: Item) -> Result<(), VaultError> {
        self.file.remove(account, item).await?;
        Err(Self::away())
    }
    async fn clear(&self) -> Result<(), VaultError> {
        self.file.clear().await?;
        Err(Self::away())
    }
}

/// A shared vault, such as one a test also looks into.
#[async_trait]
impl<V: Vault + ?Sized> Vault for std::sync::Arc<V> {
    fn name(&self) -> String {
        (**self).name()
    }
    fn note(&self) -> Option<String> {
        (**self).note()
    }
    async fn get(
        &self,
        account: &str,
        item: Item,
    ) -> Result<Option<Zeroizing<String>>, VaultError> {
        (**self).get(account, item).await
    }
    async fn set(&self, account: &str, item: Item, value: &str) -> Result<(), VaultError> {
        (**self).set(account, item, value).await
    }
    async fn remove(&self, account: &str, item: Item) -> Result<(), VaultError> {
        (**self).remove(account, item).await
    }
    async fn clear(&self) -> Result<(), VaultError> {
        (**self).clear().await
    }
}

/// For tests: nothing leaves the process.
#[derive(Debug, Default)]
pub struct MemoryVault {
    items: Mutex<HashMap<(String, Item), Zeroizing<String>>>,
}
impl MemoryVault {
    pub fn len(&self) -> usize {
        self.items.lock().expect("vault poisoned").len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
#[async_trait]
impl Vault for MemoryVault {
    fn name(&self) -> String {
        "Memory".into()
    }
    async fn get(
        &self,
        account: &str,
        item: Item,
    ) -> Result<Option<Zeroizing<String>>, VaultError> {
        Ok(self
            .items
            .lock()
            .expect("vault poisoned")
            .get(&(account.to_owned(), item))
            .cloned())
    }
    async fn set(&self, account: &str, item: Item, value: &str) -> Result<(), VaultError> {
        self.items
            .lock()
            .expect("vault poisoned")
            .insert((account.to_owned(), item), Zeroizing::new(value.to_owned()));
        Ok(())
    }
    async fn remove(&self, account: &str, item: Item) -> Result<(), VaultError> {
        self.items
            .lock()
            .expect("vault poisoned")
            .remove(&(account.to_owned(), item));
        Ok(())
    }
    async fn clear(&self) -> Result<(), VaultError> {
        self.items.lock().expect("vault poisoned").clear();
        Ok(())
    }
}

/// One file per value in a folder only the owner can open. On Windows each
/// file is sealed to the user with the system's data protection.
#[derive(Debug)]
pub struct FileVault {
    dir: PathBuf,
    note: Option<String>,
}
impl FileVault {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, note: None }
    }
    pub fn with_note(mut self, note: &str) -> Self {
        self.note = Some(note.into());
        self
    }
    fn path(&self, account: &str, item: Item) -> PathBuf {
        self.dir.join(format!("{account}.{}", item.name()))
    }
}
#[async_trait]
impl Vault for FileVault {
    fn name(&self) -> String {
        if cfg!(windows) {
            "Windows data protection".into()
        } else {
            "A private file".into()
        }
    }
    fn note(&self) -> Option<String> {
        self.note.clone()
    }
    async fn get(
        &self,
        account: &str,
        item: Item,
    ) -> Result<Option<Zeroizing<String>>, VaultError> {
        let bytes = match std::fs::read(self.path(account, item)) {
            Ok(bytes) => Zeroizing::new(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(VaultError(format!("Cannot read a kept key: {e}"))),
        };
        #[cfg(windows)]
        let bytes = platform_windows::dpapi::unprotect(&bytes)
            .map_err(|e| VaultError(format!("Cannot unseal a kept key: {e}")))?;
        String::from_utf8(bytes.to_vec())
            .map(|s| Some(Zeroizing::new(s)))
            .map_err(|_| VaultError("A kept key is damaged".into()))
    }
    async fn set(&self, account: &str, item: Item, value: &str) -> Result<(), VaultError> {
        #[cfg(windows)]
        let sealed = platform_windows::dpapi::protect(value.as_bytes())
            .map_err(|e| VaultError(format!("Cannot seal the key: {e}")))?;
        #[cfg(windows)]
        let sealed = &sealed[..];
        #[cfg(not(windows))]
        let sealed = value.as_bytes();
        crate::files::private_dir(&self.dir)
            .and_then(|()| crate::files::write_private(&self.path(account, item), sealed))
            .map_err(|e| VaultError(format!("Cannot keep the key: {e}")))
    }
    async fn remove(&self, account: &str, item: Item) -> Result<(), VaultError> {
        match std::fs::remove_file(self.path(account, item)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(VaultError(format!("Cannot remove the kept key: {e}")))
            }
            _ => Ok(()),
        }
    }
    async fn clear(&self) -> Result<(), VaultError> {
        match std::fs::remove_dir_all(&self.dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(VaultError(format!("Cannot remove the kept keys: {e}")))
            }
            _ => Ok(()),
        }
    }
}

/// The keyring, with the private file a start without an unlocked keyring
/// used instead. What is still in the file is newer than the keyring's copy:
/// it is read first and moved into the keyring. A new value replaces it, and
/// removing an item removes it from both, so a deleted key can't come back on
/// a later start without the keyring. A read that moves a value runs alone,
/// so a removal or new value can't be undone by a move begun before it.
#[derive(Debug)]
pub struct WithLeftovers<V> {
    pub keyring: V,
    pub file: FileVault,
    one_at_a_time: tokio::sync::Mutex<()>,
}
impl<V> WithLeftovers<V> {
    pub fn new(keyring: V, file: FileVault) -> Self {
        Self {
            keyring,
            file,
            one_at_a_time: tokio::sync::Mutex::new(()),
        }
    }
}
#[async_trait]
impl<V: Vault> Vault for WithLeftovers<V> {
    fn name(&self) -> String {
        self.keyring.name()
    }
    fn note(&self) -> Option<String> {
        self.keyring.note()
    }
    async fn get(
        &self,
        account: &str,
        item: Item,
    ) -> Result<Option<Zeroizing<String>>, VaultError> {
        let _alone = self.one_at_a_time.lock().await;
        let Some(left) = self.file.get(account, item).await? else {
            return self.keyring.get(account, item).await;
        };
        let moved = async {
            self.keyring.set(account, item, &left).await?;
            self.file.remove(account, item).await
        };
        if let Err(e) = moved.await {
            tracing::warn!("accounts: could not move a kept value into the keyring: {e}");
        }
        Ok(Some(left))
    }
    async fn set(&self, account: &str, item: Item, value: &str) -> Result<(), VaultError> {
        let _alone = self.one_at_a_time.lock().await;
        self.keyring.set(account, item, value).await?;
        self.file.remove(account, item).await
    }
    async fn remove(&self, account: &str, item: Item) -> Result<(), VaultError> {
        let _alone = self.one_at_a_time.lock().await;
        self.keyring.remove(account, item).await?;
        self.file.remove(account, item).await
    }
    async fn clear(&self) -> Result<(), VaultError> {
        let _alone = self.one_at_a_time.lock().await;
        self.keyring.clear().await?;
        self.file.clear().await
    }
}

#[cfg(target_os = "linux")]
pub mod secret_service {
    //! The freedesktop Secret Service (GNOME Keyring, KWallet) on the user's
    //! own session bus, with a plain session: the bus is local to the user.
    use super::{Item, Vault, VaultError};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
    use zeroize::Zeroizing;

    const DEST: &str = "org.freedesktop.secrets";
    const SERVICE: &str = "org.freedesktop.Secret.Service";
    const COLLECTION: &str = "org.freedesktop.Secret.Collection";
    const ITEM: &str = "org.freedesktop.Secret.Item";

    type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

    #[derive(Debug)]
    pub struct SecretService {
        connection: zbus::Connection,
        session: OwnedObjectPath,
        collection: OwnedObjectPath,
    }

    fn failed(e: impl std::fmt::Display) -> VaultError {
        VaultError(format!("The keyring refused: {e}"))
    }

    impl SecretService {
        /// Connects and checks that the default collection is there and unlocked.
        /// A locked keyring would need a prompt on the desktop, so it counts as
        /// unavailable rather than raising one unasked.
        pub async fn connect() -> Result<Self, VaultError> {
            let connection = zbus::Connection::session().await.map_err(failed)?;
            Self::on(connection).await
        }

        pub async fn on(connection: zbus::Connection) -> Result<Self, VaultError> {
            let reply = connection
                .call_method(
                    Some(DEST),
                    "/org/freedesktop/secrets",
                    Some(SERVICE),
                    "OpenSession",
                    &("plain", Value::from("")),
                )
                .await
                .map_err(failed)?;
            let (_, session): (OwnedValue, OwnedObjectPath) =
                reply.body().deserialize().map_err(failed)?;
            let reply = connection
                .call_method(
                    Some(DEST),
                    "/org/freedesktop/secrets",
                    Some(SERVICE),
                    "ReadAlias",
                    &("default",),
                )
                .await
                .map_err(failed)?;
            let collection: OwnedObjectPath = reply.body().deserialize().map_err(failed)?;
            if collection.as_str() == "/" {
                return Err(VaultError("The keyring has no default collection".into()));
            }
            let service = Self {
                connection,
                session,
                collection,
            };
            if service.locked(&service.collection).await? {
                return Err(VaultError("The keyring is locked".into()));
            }
            Ok(service)
        }

        async fn locked(&self, path: &ObjectPath<'_>) -> Result<bool, VaultError> {
            let reply = self
                .connection
                .call_method(
                    Some(DEST),
                    path,
                    Some("org.freedesktop.DBus.Properties"),
                    "Get",
                    &(COLLECTION, "Locked"),
                )
                .await
                .map_err(failed)?;
            let value: OwnedValue = reply.body().deserialize().map_err(failed)?;
            bool::try_from(value).map_err(failed)
        }

        fn attributes(account: &str, item: Item) -> HashMap<&str, &str> {
            HashMap::from([
                ("application", "ophio"),
                ("account", account),
                ("item", item.name()),
            ])
        }

        async fn find(
            &self,
            account: &str,
            item: Item,
        ) -> Result<Vec<OwnedObjectPath>, VaultError> {
            let (mut unlocked, locked) = self.search(Self::attributes(account, item)).await?;
            if !locked.is_empty() && unlocked.is_empty() {
                return Err(VaultError("The keyring is locked".into()));
            }
            // What Ophio kept last is in the collection it keeps to.
            unlocked.sort_by_key(|path| !self.ours(path));
            Ok(unlocked)
        }

        /// Whether `item` is in the collection Ophio keeps values in.
        fn ours(&self, item: &ObjectPath<'_>) -> bool {
            (item.as_str().strip_prefix(self.collection.as_str()))
                .is_some_and(|rest| rest.starts_with('/'))
        }

        /// The items with these attributes: those unlocked, then those locked.
        async fn search(
            &self,
            attributes: HashMap<&str, &str>,
        ) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>), VaultError> {
            let reply = self
                .connection
                .call_method(
                    Some(DEST),
                    "/org/freedesktop/secrets",
                    Some(SERVICE),
                    "SearchItems",
                    &(attributes,),
                )
                .await
                .map_err(failed)?;
            reply.body().deserialize().map_err(failed)
        }

        async fn delete(&self, items: Vec<OwnedObjectPath>) -> Result<(), VaultError> {
            for path in items {
                let reply = self
                    .connection
                    .call_method(Some(DEST), &path, Some(ITEM), "Delete", &())
                    .await
                    .map_err(failed)?;
                let prompt: OwnedObjectPath = reply.body().deserialize().map_err(failed)?;
                // Removed only once confirmed on the desktop, which Ophio
                // doesn't ask for unasked: so not removed.
                if prompt.as_str() != "/" {
                    return Err(VaultError(
                        "The keyring wants the removal confirmed. Unlock it, then try again".into(),
                    ));
                }
            }
            Ok(())
        }
    }

    #[async_trait]
    impl Vault for SecretService {
        fn name(&self) -> String {
            "Secret Service".into()
        }
        async fn get(
            &self,
            account: &str,
            item: Item,
        ) -> Result<Option<Zeroizing<String>>, VaultError> {
            let Some(path) = self.find(account, item).await?.into_iter().next() else {
                return Ok(None);
            };
            let reply = self
                .connection
                .call_method(
                    Some(DEST),
                    &path,
                    Some(ITEM),
                    "GetSecret",
                    &(&self.session,),
                )
                .await
                .map_err(failed)?;
            let (_, _, value, _): Secret = reply.body().deserialize().map_err(failed)?;
            let value = Zeroizing::new(value);
            String::from_utf8(value.to_vec())
                .map(|s| Some(Zeroizing::new(s)))
                .map_err(|_| VaultError("A kept key is damaged".into()))
        }
        async fn set(&self, account: &str, item: Item, value: &str) -> Result<(), VaultError> {
            let attributes: HashMap<String, String> = Self::attributes(account, item)
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect();
            let properties: HashMap<&str, Value<'_>> = HashMap::from([
                (
                    "org.freedesktop.Secret.Item.Label",
                    Value::from(format!("Ophio: {account} {}", item.name())),
                ),
                (
                    "org.freedesktop.Secret.Item.Attributes",
                    Value::from(attributes),
                ),
            ]);
            let secret = (
                self.session.clone(),
                Vec::<u8>::new(),
                value.as_bytes().to_vec(),
                "text/plain".to_owned(),
            );
            let reply = self
                .connection
                .call_method(
                    Some(DEST),
                    &self.collection,
                    Some(COLLECTION),
                    "CreateItem",
                    &(properties, secret, true),
                )
                .await
                .map_err(failed)?;
            let (created, prompt): (OwnedObjectPath, OwnedObjectPath) =
                reply.body().deserialize().map_err(failed)?;
            if prompt.as_str() != "/" {
                return Err(VaultError("The keyring is locked".into()));
            }
            // Copies in other collections, kept while another was the
            // default, would be read in place of this one there.
            let (unlocked, locked) = self.search(Self::attributes(account, item)).await?;
            self.delete(unlocked.into_iter().filter(|p| *p != created).collect())
                .await?;
            if !locked.is_empty() {
                return Err(VaultError(
                    "An older copy is in a locked part of the keyring. Unlock it, then try again"
                        .into(),
                ));
            }
            Ok(())
        }
        async fn remove(&self, account: &str, item: Item) -> Result<(), VaultError> {
            let (unlocked, locked) = self.search(Self::attributes(account, item)).await?;
            self.delete(unlocked).await?;
            // A copy left in a locked part would come back once it is unlocked.
            if !locked.is_empty() {
                return Err(VaultError(
                    "A copy is in a locked part of the keyring. Unlock it, then try again".into(),
                ));
            }
            Ok(())
        }
        async fn clear(&self) -> Result<(), VaultError> {
            let (unlocked, locked) = self
                .search(HashMap::from([("application", "ophio")]))
                .await?;
            self.delete(unlocked).await?;
            if !locked.is_empty() {
                return Err(VaultError(
                    "Some of Ophio's keys are in a locked part of the keyring".into(),
                ));
            }
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        //! Against a stand-in Secret Service on a private connection: the
        //! owner's keyring is never reached.
        use super::*;
        use futures_util::StreamExt;
        use std::sync::{Arc, Mutex};

        const A: &str = "/org/freedesktop/secrets/collection/a";
        const B: &str = "/org/freedesktop/secrets/collection/b";

        #[derive(Debug, Clone)]
        struct Stored {
            path: String,
            attributes: HashMap<String, String>,
            secret: Vec<u8>,
            locked: bool,
            /// Deleting it asks for confirmation on the desktop.
            prompts: bool,
        }

        #[derive(Debug, Default)]
        struct Keyring {
            default: String,
            items: Vec<Stored>,
            made: usize,
        }

        type Shared = Arc<Mutex<Keyring>>;

        /// What the stand-in answers, each with its own D-Bus signature.
        enum Answer {
            Session(OwnedValue, OwnedObjectPath),
            Path(OwnedObjectPath),
            Property(OwnedValue),
            Found(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>),
            Created(OwnedObjectPath, OwnedObjectPath),
            Secret(super::Secret),
        }

        fn path_of(p: &str) -> OwnedObjectPath {
            OwnedObjectPath::try_from(p.to_owned()).unwrap()
        }

        fn answer(
            keyring: &Shared,
            path: &str,
            member: &str,
            msg: &zbus::Message,
        ) -> Result<Answer, String> {
            let mut k = keyring.lock().unwrap();
            let body = msg.body();
            Ok(match member {
                "OpenSession" => Answer::Session(
                    Value::from("").try_to_owned().unwrap(),
                    path_of("/org/freedesktop/secrets/session/1"),
                ),
                "ReadAlias" => Answer::Path(path_of(&k.default)),
                "Get" => Answer::Property(Value::from(false).try_to_owned().unwrap()),
                "SearchItems" => {
                    let wanted: HashMap<String, String> =
                        body.deserialize().map_err(|e| e.to_string())?;
                    let found = |locked: bool| -> Vec<OwnedObjectPath> {
                        (k.items.iter())
                            .filter(|i| i.locked == locked)
                            .filter(|i| wanted.iter().all(|(n, v)| i.attributes.get(n) == Some(v)))
                            .map(|i| path_of(&i.path))
                            .collect()
                    };
                    Answer::Found(found(false), found(true))
                }
                "CreateItem" => {
                    let (properties, secret, _replace): (
                        HashMap<String, OwnedValue>,
                        super::Secret,
                        bool,
                    ) = body.deserialize().map_err(|e| e.to_string())?;
                    let attributes: HashMap<String, String> =
                        properties["org.freedesktop.Secret.Item.Attributes"]
                            .try_clone()
                            .unwrap()
                            .try_into()
                            .map_err(|e: zbus::zvariant::Error| e.to_string())?;
                    k.items.retain(|i| {
                        !(i.path.starts_with(&format!("{path}/")) && i.attributes == attributes)
                    });
                    k.made += 1;
                    let made = format!("{path}/{}", k.made);
                    k.items.push(Stored {
                        path: made.clone(),
                        attributes,
                        secret: secret.2,
                        locked: false,
                        prompts: false,
                    });
                    Answer::Created(path_of(&made), path_of("/"))
                }
                "GetSecret" => {
                    let item = (k.items.iter())
                        .find(|i| i.path == path)
                        .ok_or("no such item")?;
                    Answer::Secret((
                        path_of("/org/freedesktop/secrets/session/1"),
                        Vec::new(),
                        item.secret.clone(),
                        "text/plain".to_owned(),
                    ))
                }
                "Delete" => {
                    let at = (k.items.iter())
                        .position(|i| i.path == path)
                        .ok_or("no such item")?;
                    if k.items[at].prompts {
                        Answer::Path(path_of("/org/freedesktop/secrets/prompt/1"))
                    } else {
                        k.items.remove(at);
                        Answer::Path(path_of("/"))
                    }
                }
                other => return Err(format!("unexpected {other}")),
            })
        }

        /// A [`SecretService`] talking to a stand-in holding `keyring`.
        async fn service(keyring: Shared) -> SecretService {
            let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
            let guid = zbus::Guid::generate();
            let server = async {
                zbus::connection::Builder::unix_stream(theirs)
                    .server(guid)
                    .unwrap()
                    .p2p()
                    .build()
                    .await
                    .unwrap()
            };
            let client = async {
                zbus::connection::Builder::unix_stream(ours)
                    .p2p()
                    .build()
                    .await
                    .unwrap()
            };
            let (server, client) = tokio::join!(server, client);
            tokio::spawn(async move {
                let mut calls = zbus::MessageStream::from(&server);
                while let Some(Ok(msg)) = calls.next().await {
                    let header = msg.header();
                    if header.message_type() != zbus::message::Type::MethodCall {
                        continue;
                    }
                    let path = header.path().map(|p| p.to_string()).unwrap_or_default();
                    let member = header.member().map(|m| m.to_string()).unwrap_or_default();
                    let sent = match answer(&keyring, &path, &member, &msg) {
                        Ok(Answer::Session(output, session)) => {
                            server.reply(&header, &(output, session)).await
                        }
                        Ok(Answer::Path(at)) => server.reply(&header, &at).await,
                        Ok(Answer::Property(value)) => server.reply(&header, &value).await,
                        Ok(Answer::Found(unlocked, locked)) => {
                            server.reply(&header, &(unlocked, locked)).await
                        }
                        Ok(Answer::Created(item, prompt)) => {
                            server.reply(&header, &(item, prompt)).await
                        }
                        Ok(Answer::Secret(secret)) => server.reply(&header, &secret).await,
                        Err(e) => {
                            let name = "org.freedesktop.DBus.Error.Failed";
                            server.reply_error(&header, name, &e).await
                        }
                    };
                    sent.unwrap();
                }
            });
            SecretService::on(client).await.unwrap()
        }

        fn stored(collection: &str, n: usize, secret: &str) -> Stored {
            Stored {
                path: format!("{collection}/old{n}"),
                attributes: SecretService::attributes("groq", Item::Key)
                    .into_iter()
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .collect(),
                secret: secret.as_bytes().to_vec(),
                locked: false,
                prompts: false,
            }
        }

        #[tokio::test]
        async fn a_new_value_replaces_copies_kept_in_other_collections() {
            let keyring: Shared = Arc::new(Mutex::new(Keyring {
                default: B.into(),
                // Kept while A was the default, and found first.
                items: vec![stored(A, 1, "gsk_Old0123456789")],
                made: 0,
            }));
            let vault = service(keyring.clone()).await;
            vault
                .set("groq", Item::Key, "gsk_New0123456789")
                .await
                .unwrap();
            let read = vault.get("groq", Item::Key).await.unwrap().unwrap();
            assert_eq!(read.as_str(), "gsk_New0123456789");
            let paths: Vec<String> = (keyring.lock().unwrap().items.iter())
                .map(|i| i.path.clone())
                .collect();
            assert_eq!(paths, [format!("{B}/1")]);

            // One left in a locked collection is said, and not read.
            keyring.lock().unwrap().items.push(Stored {
                locked: true,
                ..stored(A, 2, "gsk_Locked0123456789")
            });
            let err = vault.set("groq", Item::Key, "gsk_Next0123456789").await;
            assert!(err.unwrap_err().0.contains("locked"));
            let read = vault.get("groq", Item::Key).await.unwrap().unwrap();
            assert_eq!(read.as_str(), "gsk_Next0123456789");
        }

        #[tokio::test]
        async fn a_removal_the_keyring_wants_confirmed_is_not_done() {
            let keyring: Shared = Arc::new(Mutex::new(Keyring {
                default: B.into(),
                items: vec![Stored {
                    prompts: true,
                    ..stored(B, 1, "gsk_Kept0123456789")
                }],
                made: 0,
            }));
            let vault = service(keyring.clone()).await;
            assert!(vault.remove("groq", Item::Key).await.is_err());
            assert!(vault.clear().await.is_err());
            assert_eq!(keyring.lock().unwrap().items.len(), 1);
            keyring.lock().unwrap().items[0].prompts = false;
            vault.remove("groq", Item::Key).await.unwrap();
            assert!(keyring.lock().unwrap().items.is_empty());
        }
    }
}
