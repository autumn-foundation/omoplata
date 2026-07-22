//! On-disk repository store for omoplata.
//!
//! Per the design doc's §7 crate table, this crate owns the content-addressed
//! **object store** and the **tree model** over it. [`Repository`] manages the
//! `.omoplata/` control directory and reads and writes objects addressed by a
//! hash-agile [`ObjectId`] (SHA-256 in v1). Objects are stored as loose files
//! (see `docs/adr/0002-loose-object-store.md`).

mod lock;
mod object;

pub use lock::{atomic_write, RepoLock};
pub use object::{
    Blob, EntryKind, HashAlg, Object, ObjectError, ObjectId, ObjectKind, Tree, TreeEntry,
};

use std::fs;
use std::path::{Path, PathBuf};

/// Name of the control directory placed at the root of an omoplata repository.
pub const CONTROL_DIR: &str = ".omoplata";

/// Name of the advisory lock file inside the control directory (ADR-0008).
pub const LOCK_FILE: &str = "lock";

/// Errors that can occur while operating on an omoplata store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A control directory already exists at the target root.
    #[error("an omoplata repository already exists at {0}")]
    AlreadyInitialized(PathBuf),
    /// No control directory was found at the target root.
    #[error("no omoplata repository found at {0}")]
    NotInitialized(PathBuf),
    /// The requested object is not present in the store.
    #[error("object not found: {0}")]
    ObjectNotFound(ObjectId),
    /// A stored object's bytes do not hash to the id used to look it up.
    #[error("integrity check failed for object {0}")]
    Integrity(ObjectId),
    /// An object could not be parsed.
    #[error(transparent)]
    Object(#[from] ObjectError),
    /// A filesystem operation failed.
    #[error("i/o error at {path}: {source}")]
    Io {
        /// Path being operated on when the error occurred.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

fn io(path: impl AsRef<Path>) -> impl FnOnce(std::io::Error) -> StoreError {
    let path = path.as_ref().to_path_buf();
    move |source| StoreError::Io { path, source }
}

/// A handle to an omoplata repository rooted at `root`.
#[derive(Debug, Clone)]
pub struct Repository {
    root: PathBuf,
}

impl Repository {
    /// The repository root (the directory that contains `.omoplata/`).
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path to the control directory.
    #[must_use]
    pub fn control_dir(&self) -> PathBuf {
        self.root.join(CONTROL_DIR)
    }

    /// Initialize a new omoplata repository at `root`.
    ///
    /// Creates the `.omoplata/` control directory with an `objects/` store,
    /// a `refs/` directory, and a `config` file.
    ///
    /// # Errors
    /// Returns [`StoreError::AlreadyInitialized`] if a control directory is
    /// already present, or [`StoreError::Io`] on any filesystem failure.
    pub fn init(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let root = root.as_ref().to_path_buf();
        let control = root.join(CONTROL_DIR);
        if control.exists() {
            return Err(StoreError::AlreadyInitialized(root));
        }
        fs::create_dir_all(control.join("objects")).map_err(io(control.join("objects")))?;
        fs::create_dir_all(control.join("refs")).map_err(io(control.join("refs")))?;
        fs::write(control.join("config"), "[core]\nformat_version = 0\n")
            .map_err(io(control.join("config")))?;
        Ok(Self { root })
    }

    /// Open an existing omoplata repository rooted at `root`.
    ///
    /// # Errors
    /// Returns [`StoreError::NotInitialized`] if no control directory exists.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let root = root.as_ref().to_path_buf();
        if root.join(CONTROL_DIR).is_dir() {
            Ok(Self { root })
        } else {
            Err(StoreError::NotInitialized(root))
        }
    }

    /// Whether an omoplata repository exists at `root`.
    #[must_use]
    pub fn exists(root: impl AsRef<Path>) -> bool {
        root.as_ref().join(CONTROL_DIR).is_dir()
    }

    /// Path to the advisory lock file (`.omoplata/lock`).
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.control_dir().join(LOCK_FILE)
    }

    /// Take an **exclusive**, blocking advisory lock on the repository's mutable
    /// state, returning a guard that releases it on drop (ADR-0008).
    ///
    /// Hold the returned [`RepoLock`] across a whole `load -> mutate -> save`
    /// critical section (ref updates, op-log appends, undo, auto-rebase) so the
    /// read-modify-write cycle is atomic with respect to every other `omo`
    /// process. Blocks until the lock is acquired; the lock is also released
    /// automatically if the process dies while holding it, so there is no stale
    /// lock to clean up. Read-only callers do not need the lock, because
    /// [`atomic_write`] guarantees they never observe a torn file.
    ///
    /// # Errors
    ///
    /// [`StoreError::Io`] if the lock file cannot be opened or locked.
    pub fn lock(&self) -> Result<RepoLock, StoreError> {
        RepoLock::acquire(self.lock_path())
    }

    /// Try to take the exclusive advisory lock **without blocking** (ADR-0008).
    ///
    /// Returns `Ok(Some(guard))` if the lock was free and is now held, or
    /// `Ok(None)` if another process currently holds it — letting a caller
    /// report "repository busy" instead of waiting.
    ///
    /// # Errors
    ///
    /// [`StoreError::Io`] if the lock file cannot be opened, or locking fails for
    /// a reason other than the lock being held.
    pub fn try_lock(&self) -> Result<Option<RepoLock>, StoreError> {
        RepoLock::try_acquire(self.lock_path())
    }

    /// Loose-object path for `id`: `.omoplata/objects/<alg>/<xx>/<rest>`.
    fn object_path(&self, id: &ObjectId) -> PathBuf {
        let hex = id.hex();
        let (shard, rest) = hex.split_at(2);
        self.control_dir()
            .join("objects")
            .join(id.alg().as_str())
            .join(shard)
            .join(rest)
    }

    /// Write an object into the store, returning its content address.
    ///
    /// Idempotent: an already-present object is left untouched. The write is
    /// crash-atomic (staged to a temp file, `fsync`ed, and `rename`d into place,
    /// then the directory `fsync`ed — see [`atomic_write`]) so readers never
    /// observe a partial object and a crash cannot leave a torn one.
    ///
    /// # Errors
    /// Returns [`StoreError::Io`] on any filesystem failure.
    pub fn write_object(&self, object: &Object) -> Result<ObjectId, StoreError> {
        let id = object.id();
        let hex = id.hex();
        let (shard, rest) = hex.split_at(2);
        let dir = self
            .control_dir()
            .join("objects")
            .join(id.alg().as_str())
            .join(shard);
        let path = dir.join(rest);
        if path.exists() {
            return Ok(id);
        }
        fs::create_dir_all(&dir).map_err(io(&dir))?;
        atomic_write(&path, &object.serialize())?;
        Ok(id)
    }

    /// Read and verify an object by its content address.
    ///
    /// # Errors
    /// Returns [`StoreError::ObjectNotFound`] if absent, [`StoreError::Integrity`]
    /// if the stored bytes do not hash to `id`, [`StoreError::Object`] if the
    /// bytes are malformed, or [`StoreError::Io`] on filesystem failure.
    pub fn read_object(&self, id: &ObjectId) -> Result<Object, StoreError> {
        let path = self.object_path(id);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::ObjectNotFound(id.clone()));
            }
            Err(source) => return Err(StoreError::Io { path, source }),
        };
        let object = Object::deserialize(&bytes)?;
        if &object.id_with(id.alg()) != id {
            return Err(StoreError::Integrity(id.clone()));
        }
        Ok(object)
    }

    /// Whether an object with `id` is present.
    #[must_use]
    pub fn has_object(&self, id: &ObjectId) -> bool {
        self.object_path(id).exists()
    }

    /// Convenience: store raw bytes as a blob and return its id.
    ///
    /// # Errors
    /// Returns [`StoreError::Io`] on any filesystem failure.
    pub fn write_blob(&self, bytes: impl Into<Vec<u8>>) -> Result<ObjectId, StoreError> {
        self.write_object(&Object::Blob(Blob::new(bytes)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn init_creates_control_dir() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        assert!(repo.control_dir().is_dir());
        assert!(repo.control_dir().join("objects").is_dir());
        assert!(repo.control_dir().join("refs").is_dir());
        assert!(repo.control_dir().join("config").is_file());
        assert!(Repository::exists(dir.path()));
    }

    #[test]
    fn init_twice_errors() {
        let dir = tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        let err = Repository::init(dir.path()).unwrap_err();
        assert!(matches!(err, StoreError::AlreadyInitialized(_)));
    }

    #[test]
    fn open_missing_errors() {
        let dir = tempdir().unwrap();
        let err = Repository::open(dir.path()).unwrap_err();
        assert!(matches!(err, StoreError::NotInitialized(_)));
    }

    #[test]
    fn blob_write_read_roundtrip() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let id = repo.write_blob(b"hello omoplata".to_vec()).unwrap();
        assert!(repo.has_object(&id));
        match repo.read_object(&id).unwrap() {
            Object::Blob(b) => assert_eq!(b.bytes(), b"hello omoplata"),
            other => panic!("expected blob, got {other:?}"),
        }
    }

    #[test]
    fn write_object_is_idempotent() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = repo.write_blob(b"x".to_vec()).unwrap();
        let b = repo.write_blob(b"x".to_vec()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn read_missing_object_errors() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let id = Object::Blob(Blob::new(b"nope".to_vec())).id();
        assert!(matches!(
            repo.read_object(&id),
            Err(StoreError::ObjectNotFound(_))
        ));
        assert!(!repo.has_object(&id));
    }

    #[test]
    fn tampered_object_fails_integrity() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let id = repo.write_blob(b"hello".to_vec()).unwrap();
        let hex = id.hex();
        let (shard, rest) = hex.split_at(2);
        let path = repo
            .control_dir()
            .join("objects")
            .join(id.alg().as_str())
            .join(shard)
            .join(rest);
        fs::write(&path, b"blob 5\0HELLO").unwrap();
        assert!(matches!(
            repo.read_object(&id),
            Err(StoreError::Integrity(_))
        ));
    }

    #[test]
    fn tree_write_read_roundtrip() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let blob_id = repo.write_blob(b"fn main() {}".to_vec()).unwrap();
        let mut tree = Tree::new();
        tree.insert("main.rs", EntryKind::Blob, blob_id.clone())
            .unwrap();
        let tree_id = repo.write_object(&Object::Tree(tree)).unwrap();
        match repo.read_object(&tree_id).unwrap() {
            Object::Tree(t) => {
                let e = t.get("main.rs").unwrap();
                assert_eq!(e.id, blob_id);
                assert_eq!(e.kind, EntryKind::Blob);
            }
            other => panic!("expected tree, got {other:?}"),
        }
    }
}
