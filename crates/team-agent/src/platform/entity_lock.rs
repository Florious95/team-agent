//! Reentrant ownership of an already-acquired, entity-scoped file lock.
//! Only live guards are indexed; this is not a cache of runtime observations.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

thread_local! {
    static HELD: RefCell<BTreeMap<PathBuf, Weak<EntityLock>>> = const { RefCell::new(BTreeMap::new()) };
}

pub(crate) struct EntityLock {
    file: File,
    key: PathBuf,
}

impl EntityLock {
    pub(crate) fn current(path: &Path) -> Option<Rc<Self>> {
        let key = path.canonicalize().ok()?;
        HELD.with(|held| held.borrow().get(&key).and_then(Weak::upgrade))
    }

    /// Caller has successfully acquired the OS lock on this exact file.
    pub(crate) fn acquired(path: &Path, file: File) -> std::io::Result<Rc<Self>> {
        let key = path.canonicalize()?;
        let lock = Rc::new(Self {
            file,
            key: key.clone(),
        });
        HELD.with(|held| {
            held.borrow_mut().insert(key, Rc::downgrade(&lock));
        });
        Ok(lock)
    }
}

impl Drop for EntityLock {
    fn drop(&mut self) {
        HELD.with(|held| {
            held.borrow_mut().remove(&self.key);
        });
        let _ = super::file_lock::unlock(&self.file);
        // File closure also releases the lock if explicit unlock fails.
    }
}
