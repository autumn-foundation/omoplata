//! Named remotes and object replication — Phase 1 of distributed omoplata
//! (ADR-0010).
//!
//! A [`Remote`] is a name bound to the path of another `.omoplata` repository.
//! [`copy_closure`] replicates a content-addressed object graph from one store
//! into another; because equal content has an equal id, the copy is trivially
//! idempotent and needs no manifest or negotiation — it simply writes whatever
//! the destination is missing.
//!
//! This is deliberately the *local-path* transport: a remote is reachable as a
//! filesystem path (a shared mount, an export, a sibling clone). Networked
//! transports and a remote landing authority are the later phases in ADR-0010;
//! the registry and the closure copy are the seams they build on.

use std::path::{Path, PathBuf};

use omoplata_store::{atomic_write, Object, ObjectId, Repository};
use serde::{Deserialize, Serialize};

use crate::error::WorkError;

/// A registered remote: a name and the path to another omoplata repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remote {
    /// The remote's unique name (e.g. `origin`).
    pub name: String,
    /// Path to the remote's repository root (the dir holding `.omoplata`).
    pub path: PathBuf,
}

/// The set of remotes registered for a repository, persisted at
/// `.omoplata/remotes.json` (mirrors [`QueueRegistry`]/[`WorkspaceRegistry`]).
///
/// [`QueueRegistry`]: crate::QueueRegistry
/// [`WorkspaceRegistry`]: crate::WorkspaceRegistry
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRegistry {
    remotes: Vec<Remote>,
}

impl RemoteRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The canonical registry path: `.omoplata/remotes.json`.
    #[must_use]
    pub fn path_in(repo: &Repository) -> PathBuf {
        repo.control_dir().join("remotes.json")
    }

    /// Every registered remote, in registration order.
    #[must_use]
    pub fn remotes(&self) -> &[Remote] {
        &self.remotes
    }

    /// Borrow a remote by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Remote> {
        self.remotes.iter().find(|r| r.name == name)
    }

    /// Register a new remote.
    ///
    /// # Errors
    ///
    /// [`WorkError::RemoteExists`] if a remote with `name` is already
    /// registered.
    pub fn add(&mut self, name: impl Into<String>, path: PathBuf) -> Result<&Remote, WorkError> {
        let name = name.into();
        if self.get(&name).is_some() {
            return Err(WorkError::RemoteExists(name));
        }
        self.remotes.push(Remote { name, path });
        let idx = self.remotes.len() - 1;
        Ok(&self.remotes[idx])
    }

    /// Remove a remote by name, returning it.
    ///
    /// # Errors
    ///
    /// [`WorkError::UnknownRemote`] if no remote with `name` is registered.
    pub fn remove(&mut self, name: &str) -> Result<Remote, WorkError> {
        let idx = self
            .remotes
            .iter()
            .position(|r| r.name == name)
            .ok_or_else(|| WorkError::UnknownRemote(name.to_owned()))?;
        Ok(self.remotes.remove(idx))
    }

    /// Persist the registry to `path` as pretty JSON, crash-atomically (temp
    /// file → `fsync` → `rename` → dir `fsync`).
    ///
    /// # Errors
    ///
    /// [`WorkError::Decode`] on a serialization failure (never expected), or
    /// [`WorkError::Store`] on a filesystem failure.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), WorkError> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| WorkError::Decode(e.to_string()))?;
        atomic_write(path.as_ref(), &json)?;
        Ok(())
    }

    /// Load a registry from `path`; a missing file yields an empty registry so
    /// callers can create it lazily.
    ///
    /// # Errors
    ///
    /// [`WorkError::Io`] on a filesystem failure other than "not found", or
    /// [`WorkError::Decode`] if the file is not valid registry JSON.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, WorkError> {
        let path = path.as_ref();
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(source) => {
                return Err(WorkError::Io {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        serde_json::from_slice(&bytes).map_err(|e| WorkError::Decode(e.to_string()))
    }

    /// Locked, crash-atomic read-modify-write on the repository's remote
    /// registry, mirroring [`WorkspaceRegistry::mutate_locked`].
    ///
    /// # Errors
    ///
    /// [`WorkError::Store`] if the lock cannot be acquired, any error `f`
    /// returns, or an I/O/decode error from the load or save.
    ///
    /// [`WorkspaceRegistry::mutate_locked`]: crate::WorkspaceRegistry::mutate_locked
    pub fn mutate_locked<F, T>(repo: &Repository, f: F) -> Result<T, WorkError>
    where
        F: FnOnce(&mut RemoteRegistry) -> Result<T, WorkError>,
    {
        let _guard = repo.lock()?;
        let path = Self::path_in(repo);
        let mut registry = RemoteRegistry::load(&path)?;
        let out = f(&mut registry)?;
        registry.save(&path)?;
        Ok(out)
    }
}

/// Copy the object closure rooted at `root` from `from` into `to`, writing any
/// object `to` is missing. Returns the number of objects actually written.
///
/// The graph is content-addressed, so this is idempotent and needs no
/// negotiation: a re-fetch copies nothing, and two remotes that share content
/// share ids. Every reachable object (the root tree, its subtrees, and every
/// blob) is visited; a blob is a leaf, a tree enqueues its entries.
///
/// # Errors
///
/// [`WorkError::Store`] if an object cannot be read from `from` or written to
/// `to`.
pub fn copy_closure(
    from: &Repository,
    to: &Repository,
    root: &ObjectId,
) -> Result<usize, WorkError> {
    let mut copied = 0;
    let mut stack = vec![root.clone()];
    while let Some(id) = stack.pop() {
        let object = from.read_object(&id)?;
        if let Object::Tree(tree) = &object {
            for entry in tree.entries() {
                stack.push(entry.id.clone());
            }
        }
        if !to.has_object(&id) {
            to.write_object(&object)?;
            copied += 1;
        }
    }
    Ok(copied)
}
