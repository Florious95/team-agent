//! Entity-locked file projections. A commit record is published only after all
//! replacement cells are durable. Readers of an uncommitted cell see its prior
//! value, so a killed writer cannot expose half a lifecycle transition.
use super::Error;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: u64 = 16 * 1024 * 1024;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct Key {
    collection: String,
    id: String,
}
impl Key {
    pub(crate) fn new(collection: &str, id: impl AsRef<str>) -> Self {
        Self {
            collection: collection.into(),
            id: id.as_ref().into(),
        }
    }
    fn name(&self) -> String {
        digest(self.id.as_bytes())
    }
    fn validate(&self) -> Result<(), Error> {
        if self.collection.is_empty()
            || !self
                .collection
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c == b'_')
            || self.id.is_empty()
        {
            return Err(Error::Fence);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
enum Stored {
    Present(Value),
    Deleted,
}
impl Stored {
    fn into_option(self) -> Option<Value> {
        match self {
            Self::Present(value) => Some(value),
            Self::Deleted => None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Previous {
    revision: u64,
    value: Stored,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Cell {
    v: u8,
    key: Key,
    revision: u64,
    transaction: String,
    value: Stored,
    previous: Previous,
}
#[derive(Serialize, Deserialize)]
struct Commit {
    v: u8,
    transaction: String,
    cells: BTreeMap<String, String>,
}

pub(crate) trait ReadRecords {
    fn get<T: DeserializeOwned>(&self, key: &Key) -> Result<Option<T>, Error>;
    fn list<T: DeserializeOwned>(&self, collection: &str) -> Result<Vec<T>, Error>;
}
pub(crate) struct Projections {
    root: PathBuf,
    readonly: bool,
}
impl ReadRecords for Projections {
    fn get<T: DeserializeOwned>(&self, key: &Key) -> Result<Option<T>, Error> {
        Projections::get(self, key)
    }
    fn list<T: DeserializeOwned>(&self, collection: &str) -> Result<Vec<T>, Error> {
        Projections::list(self, collection)
    }
}
impl ReadRecords for WriteBatch<'_> {
    fn get<T: DeserializeOwned>(&self, key: &Key) -> Result<Option<T>, Error> {
        WriteBatch::get(self, key)
    }
    fn list<T: DeserializeOwned>(&self, collection: &str) -> Result<Vec<T>, Error> {
        WriteBatch::list(self, collection)
    }
}
pub(crate) struct WriteBatch<'a> {
    store: &'a Projections,
    // Dropping Files releases the OS locks, including on process exit.
    _locks: Vec<File>,
    allowed: BTreeSet<Key>,
    writes: BTreeMap<Key, Stored>,
}
impl Projections {
    pub(crate) fn new(root: &Path, readonly: bool) -> Self {
        Self {
            root: root.join("projection"),
            readonly,
        }
    }
    pub(crate) fn initialize(&self) -> Result<(), Error> {
        for name in ["cells", "locks", "commits"] {
            private_dir(&self.root.join(name))?;
        }
        Ok(())
    }
    fn path(&self, key: &Key) -> Result<PathBuf, Error> {
        key.validate()?;
        Ok(self
            .root
            .join("cells")
            .join(&key.collection)
            .join(format!("{}.json", key.name())))
    }
    fn committed(&self, key: &Key) -> Result<Previous, Error> {
        let Some(cell) = read_json::<Cell>(&self.path(key)?)? else {
            return Ok(Previous {
                revision: 0,
                value: Stored::Deleted,
            });
        };
        if cell.v != 1
            || cell.key != *key
            || cell.revision
                != cell
                    .previous
                    .revision
                    .checked_add(1)
                    .ok_or(Error::Corrupt)?
            || !valid_nonce(&cell.transaction)
        {
            return Err(Error::Corrupt);
        }
        let commit = read_json::<Commit>(
            &self
                .root
                .join("commits")
                .join(format!("{}.json", cell.transaction)),
        )?;
        match commit {
            None => Ok(cell.previous),
            Some(commit) => {
                let address = format!("{}/{}", key.collection, key.name());
                if commit.v != 1
                    || commit.transaction != cell.transaction
                    || commit.cells.get(&address) != Some(&digest(&serde_json::to_vec(&cell)?))
                {
                    return Err(Error::Corrupt);
                }
                Ok(Previous {
                    revision: cell.revision,
                    value: cell.value,
                })
            }
        }
    }
    pub(crate) fn get<T: DeserializeOwned>(&self, key: &Key) -> Result<Option<T>, Error> {
        Ok(self.versioned(key)?.1)
    }
    pub(crate) fn versioned<T: DeserializeOwned>(
        &self,
        key: &Key,
    ) -> Result<(u64, Option<T>), Error> {
        let cell = self.committed(key)?;
        Ok((
            cell.revision,
            cell.value
                .into_option()
                .map(serde_json::from_value)
                .transpose()?,
        ))
    }
    pub(crate) fn list<T: DeserializeOwned>(&self, collection: &str) -> Result<Vec<T>, Error> {
        Key::new(collection, "validation").validate()?;
        let dir = self.root.join("cells").join(collection);
        if dir.exists() {
            validate_ancestors(&dir)?;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut keys = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.path().extension().is_some_and(|v| v == "json") {
                let cell: Cell = read_json(&entry.path())?.ok_or(Error::Corrupt)?;
                if cell.key.collection != collection || self.path(&cell.key)? != entry.path() {
                    return Err(Error::Corrupt);
                }
                keys.push(cell.key);
            }
        }
        keys.sort();
        keys.into_iter()
            .filter_map(|key| self.get(&key).transpose())
            .collect()
    }
    pub(crate) fn write(
        &self,
        keys: impl IntoIterator<Item = Key>,
    ) -> Result<WriteBatch<'_>, Error> {
        if self.readonly {
            return Err(Error::Invalid("read-only contract projection"));
        }
        let allowed: BTreeSet<Key> = keys.into_iter().collect();
        let mut locks = Vec::new();
        for key in &allowed {
            key.validate()?;
            let dir = self.root.join("locks").join(&key.collection);
            private_dir(&dir)?;
            let path = dir.join(format!("{}.lock", key.name()));
            regular_or_absent(&path)?;
            let mut options = OpenOptions::new();
            options.read(true).write(true).create(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options
                    .mode(0o600)
                    .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
            }
            let file = options.open(path)?;
            file.lock()?;
            locks.push(file);
        }
        Ok(WriteBatch {
            store: self,
            _locks: locks,
            allowed,
            writes: BTreeMap::new(),
        })
    }
}
impl WriteBatch<'_> {
    pub(crate) fn expect_revision(&self, key: &Key, expected: u64) -> Result<(), Error> {
        if !self.allowed.contains(key) || self.store.committed(key)?.revision != expected {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    pub(crate) fn get<T: DeserializeOwned>(&self, key: &Key) -> Result<Option<T>, Error> {
        match self.writes.get(key) {
            Some(value) => value
                .clone()
                .into_option()
                .map(serde_json::from_value)
                .transpose()
                .map_err(Into::into),
            None => self.store.get(key),
        }
    }
    pub(crate) fn list<T: DeserializeOwned>(&self, collection: &str) -> Result<Vec<T>, Error> {
        self.store.list(collection)
    }
    pub(crate) fn put<T: Serialize>(&mut self, key: Key, value: &T) -> Result<(), Error> {
        if !self.allowed.contains(&key) {
            return Err(Error::Fence);
        }
        self.writes
            .insert(key, Stored::Present(serde_json::to_value(value)?));
        Ok(())
    }
    pub(crate) fn delete(&mut self, key: Key) -> Result<(), Error> {
        if !self.allowed.contains(&key) {
            return Err(Error::Fence);
        }
        self.writes.insert(key, Stored::Deleted);
        Ok(())
    }
    pub(crate) fn commit(self) -> Result<(), Error> {
        if self.writes.is_empty() {
            return Ok(());
        }
        let transaction = nonce();
        let mut cells = BTreeMap::new();
        for (key, value) in self.writes {
            let previous = self.store.committed(&key)?;
            let cell = Cell {
                v: 1,
                revision: previous.revision.checked_add(1).ok_or(Error::Corrupt)?,
                previous,
                key: key.clone(),
                transaction: transaction.clone(),
                value,
            };
            let bytes = serde_json::to_vec(&cell)?;
            cells.insert(format!("{}/{}", key.collection, key.name()), digest(&bytes));
            atomic_bytes(&self.store.path(&key)?, &bytes)?;
        }
        let commit = Commit {
            v: 1,
            transaction: transaction.clone(),
            cells,
        };
        // Append-only commit record: a collision must fail, never replace history.
        immutable_json(
            &self
                .store
                .root
                .join("commits")
                .join(format!("{transaction}.json")),
            &commit,
        )
    }
}

pub(crate) fn nonce() -> String {
    let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    digest(format!("{}:{time}:{n}", std::process::id()).as_bytes())
}
fn valid_nonce(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, Error> {
    if !regular_or_absent(path)? {
        return Ok(None);
    }
    validate_ancestors(path.parent().ok_or(Error::Fence)?)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error::Corrupt);
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}
pub(crate) fn immutable_json(path: &Path, value: &impl Serialize) -> Result<(), Error> {
    let bytes = serde_json::to_vec(value)?;
    let temp = write_temp(path, &bytes)?;
    let result = std::fs::hard_link(&temp, path)
        .map_err(Error::from)
        .and_then(|_| sync_dir(path.parent().ok_or(Error::Fence)?));
    let _ = std::fs::remove_file(temp);
    result
}
fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    regular_or_absent(path)?;
    let temp = write_temp(path, bytes)?;
    let result = std::fs::rename(&temp, path)
        .map_err(Error::from)
        .and_then(|_| sync_dir(path.parent().ok_or(Error::Fence)?));
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}
fn write_temp(path: &Path, bytes: &[u8]) -> Result<PathBuf, Error> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error::Invalid("projection size"));
    }
    let parent = path.parent().ok_or(Error::Fence)?;
    private_dir(parent)?;
    let temp = parent.join(format!(".{}.tmp", nonce()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
    }
    let mut file = options.open(&temp)?;
    if let Err(e) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = std::fs::remove_file(&temp);
        return Err(e.into());
    }
    Ok(temp)
}
fn regular_or_absent(path: &Path) -> Result<bool, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(Error::Fence),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
pub(crate) fn validate_ancestors(path: &Path) -> Result<(), Error> {
    for ancestor in path.ancestors() {
        let meta = std::fs::symlink_metadata(ancestor)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(Error::Fence);
        }
    }
    Ok(())
}
fn private_dir(path: &Path) -> Result<(), Error> {
    if path.exists() {
        return validate_ancestors(path);
    }
    let parent = path.parent().ok_or(Error::Fence)?;
    if !parent.exists() {
        private_dir(parent)?;
    }
    validate_ancestors(parent)?;
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => sync_dir(parent),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => validate_ancestors(path),
        Err(e) => Err(e.into()),
    }
}
fn sync_dir(path: &Path) -> Result<(), Error> {
    File::open(path)?.sync_all().map_err(Into::into)
}
