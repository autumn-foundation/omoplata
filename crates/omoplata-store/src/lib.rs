//! On-disk repository store for omoplata.
//!
//! This is an early scaffold: it manages the `.omoplata/` control directory
//! that later tiers (store, algebra, git interop) will build on.

use std::fs;
use std::path::{Path, PathBuf};

/// Name of the control directory placed at the root of an omoplata repository.
pub const CONTROL_DIR: &str = ".omoplata";

/// Errors that can occur while operating on an omoplata store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A control directory already exists at the target root.
    #[error("an omoplata repository already exists at {0}")]
    AlreadyInitialized(PathBuf),
    /// No control directory was found at the target root.
    #[error("no omoplata repository found at {0}")]
    NotInitialized(PathBuf),
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

fn io(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> StoreError {
    let path = path.into();
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
}
