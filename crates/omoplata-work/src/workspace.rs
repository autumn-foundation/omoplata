//! **Workspaces** — multiple working copies over one shared `.omoplata`
//! (design-doc milestone **M2**, §5.1, principle **P4**).
//!
//! # What the design doc asks for
//!
//! The design doc specifies the working-copy model but does not pin a
//! multi-working-directory *workspace* registry; the relevant text is:
//!
//! **§9 M2** — *"`omoplata-identity` + `omoplata-work`: init/commit/rebase/undo
//! on a real repo; conflicts ride through rebases."* — this module supplies the
//! **commit** half of that milestone (the everyday-loop `omo commit`).
//!
//! **§3 P4 — No index, no stash, universal undo:** *"Working copy is a commit,
//! auto-snapshotted. Every repository mutation is an entry in the operation
//! log."* — a workspace's working directory is snapshotted into a `tree` object
//! and that tree *is* the commit ([`snapshot`]); every commit/switch is one
//! op-log entry ([`OpKind::Commit`] / [`OpKind::Switch`]).
//!
//! **§5.1 — Objects and snapshots:** *"Content-addressed blobs and trees,
//! SHA-256 … Tree-at-time-T is O(path) as in git."* — [`snapshot`] and
//! [`materialize`] are the filesystem ⇄ tree bridge over that store.
//!
//! **§10 Prior Art (Jujutsu):** the doc adopts jj's *"WC-as-commit … op log"*.
//! Where the doc is silent on the multi-working-copy shape, this module follows
//! the **jj workspace model**: a workspace is its own working directory plus its
//! own current-change pointer, and every workspace shares one object store, one
//! op log, and one set of refs under a single `.omoplata`. Workspace operations
//! append **workspace-scoped** ops to that shared log.
//!
//! # Model
//!
//! A [`Workspace`] is `{ name, working_dir, change }`: a stable
//! [`ChangeId`](omoplata_identity::ChangeId) minted once at registration, whose
//! **tip** is tracked in the shared op log's ref map (keyed by the change id, as
//! [`OpKind::Commit`]/[`OpKind::Switch`]/[`OpKind::Rebase`] all fold). The
//! [`WorkspaceRegistry`] is the set of workspaces, persisted at
//! `.omoplata/workspaces.json` and mutated only under the repository lock via
//! [`WorkspaceRegistry::mutate_locked`] so concurrent `omo` processes always see
//! a consistent set.
//!
//! # Concurrency
//!
//! Every mutation of shared state holds [`Repository::lock`]: registry edits go
//! through [`WorkspaceRegistry::mutate_locked`], and commit/switch go through
//! [`OpLog::mutate_locked`]. Reads (listing workspaces, reading the op log,
//! reading objects) are lock-free because [`atomic_write`] guarantees no reader
//! ever observes a torn file. The object store itself is already concurrency
//! safe (content-addressed, atomic, idempotent writes), so [`snapshot`] and
//! [`materialize`] need no additional locking.
//!
//! No `unwrap`/`expect`/`panic` appears in non-test code; every fallible step
//! returns [`WorkError`].
//!
//! [`atomic_write`]: omoplata_store::atomic_write
//! [`Repository::lock`]: omoplata_store::Repository::lock

use std::path::{Path, PathBuf};

use omoplata_identity::{ChangeId, CommitId};
use omoplata_store::{atomic_write, EntryKind, Object, ObjectId, Repository, Tree};
use serde::{Deserialize, Serialize};

use crate::error::WorkError;

/// Control-directory names that a snapshot never descends into and a checkout
/// never removes: the omoplata store and any colocated git directory.
const CONTROL_NAMES: [&str; 2] = [omoplata_store::CONTROL_DIR, ".git"];

/// A single registered workspace: a working directory plus its current change.
///
/// The `change` is a stable [`ChangeId`] minted at registration; its **tip**
/// (the commit the working directory currently reflects) lives in the shared op
/// log's ref map keyed by `change`, so it advances atomically with every
/// [`OpKind::Commit`] and [`OpKind::Switch`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// The workspace's unique, human-facing name (e.g. `"w1"`).
    pub name: String,
    /// The absolute path to this workspace's working directory.
    pub working_dir: PathBuf,
    /// The stable change whose tip this workspace tracks.
    pub change: ChangeId,
}

/// The set of registered workspaces, persisted in the shared `.omoplata`.
///
/// Order is preserved by registration order; names are unique. Persisted as
/// pretty JSON at [`WorkspaceRegistry::path_in`] and mutated only under the
/// repository lock ([`mutate_locked`](WorkspaceRegistry::mutate_locked)).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRegistry {
    workspaces: Vec<Workspace>,
}

impl WorkspaceRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The canonical registry path: `.omoplata/workspaces.json`.
    #[must_use]
    pub fn path_in(repo: &Repository) -> PathBuf {
        repo.control_dir().join("workspaces.json")
    }

    /// Every registered workspace, in registration order.
    #[must_use]
    pub fn workspaces(&self) -> &[Workspace] {
        &self.workspaces
    }

    /// Borrow a workspace by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.name == name)
    }

    /// Register a new workspace, minting a fresh current-change pointer.
    ///
    /// The change id is derived from the (unique) workspace name so that it is
    /// stable and legible; each workspace therefore advances an independent ref,
    /// which is what keeps concurrent commits in different workspaces from
    /// clobbering one another.
    ///
    /// # Errors
    ///
    /// [`WorkError::WorkspaceExists`] if a workspace with `name` is already
    /// registered.
    pub fn add(
        &mut self,
        name: impl Into<String>,
        working_dir: PathBuf,
    ) -> Result<&Workspace, WorkError> {
        let name = name.into();
        if self.get(&name).is_some() {
            return Err(WorkError::WorkspaceExists(name));
        }
        let change = ChangeId::new(format!("ws/{name}"));
        self.workspaces.push(Workspace {
            name,
            working_dir,
            change,
        });
        // `push` guarantees a last element; index is in range.
        let idx = self.workspaces.len() - 1;
        Ok(&self.workspaces[idx])
    }

    /// Remove a workspace by name, returning it.
    ///
    /// This drops the registry entry only; the shared op log (and therefore the
    /// change's recorded history) is left intact — undo still tells the truth.
    ///
    /// # Errors
    ///
    /// [`WorkError::UnknownWorkspace`] if no workspace with `name` is registered.
    pub fn remove(&mut self, name: &str) -> Result<Workspace, WorkError> {
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.name == name)
            .ok_or_else(|| WorkError::UnknownWorkspace(name.to_owned()))?;
        Ok(self.workspaces.remove(idx))
    }

    /// Persist the registry to `path` as pretty JSON, crash-atomically.
    ///
    /// The write goes through [`atomic_write`] (temp file → `fsync` → `rename` →
    /// directory `fsync`), so a reader always observes either the complete
    /// previous registry or the complete new one — never a torn file.
    ///
    /// # Errors
    ///
    /// [`WorkError::Decode`] if serialization fails (never expected), or
    /// [`WorkError::Store`] on any filesystem failure.
    ///
    /// [`atomic_write`]: omoplata_store::atomic_write
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), WorkError> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| WorkError::Decode(e.to_string()))?;
        atomic_write(path.as_ref(), &json)?;
        Ok(())
    }

    /// Load a registry from `path`. A missing file yields an empty registry so
    /// callers can create it lazily.
    ///
    /// Because [`save`](Self::save) publishes via an atomic `rename`, `load`
    /// never observes a partially written registry.
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

    /// Perform a **locked, crash-atomic read-modify-write** on the repository's
    /// workspace registry, mirroring [`OpLog::mutate_locked`].
    ///
    /// Acquires the repository's exclusive advisory lock, loads the registry,
    /// runs `f`, saves it atomically, and releases the lock — all as one
    /// critical section, so two concurrent `omo` processes serialize and neither
    /// loses the other's registry update. `f`'s return value is passed back once
    /// the registry has been persisted.
    ///
    /// # Errors
    ///
    /// [`WorkError::Store`] if the lock cannot be acquired, any error `f`
    /// returns, or [`WorkError::Io`]/[`WorkError::Decode`] from the load/save.
    ///
    /// [`OpLog::mutate_locked`]: crate::OpLog::mutate_locked
    pub fn mutate_locked<F, T>(repo: &Repository, f: F) -> Result<T, WorkError>
    where
        F: FnOnce(&mut WorkspaceRegistry) -> Result<T, WorkError>,
    {
        // Held for the whole load -> mutate -> save critical section; released on
        // drop (and, inherently, on process death).
        let _guard = repo.lock()?;
        let path = Self::path_in(repo);
        let mut registry = WorkspaceRegistry::load(&path)?;
        let out = f(&mut registry)?;
        registry.save(&path)?;
        Ok(out)
    }
}

/// Recursively **snapshot** a working directory into an omoplata [`Tree`],
/// storing every blob and subtree in `repo`'s object store and returning the
/// root tree's [`ObjectId`] (§5.1).
///
/// The control directories (`.omoplata`, `.git`) are never descended into.
/// Regular files become blobs; subdirectories become subtrees. Because
/// [`Tree`] keeps its entries sorted, the resulting tree id is independent of
/// directory-iteration order — the same working-copy content always yields the
/// same commit id (content-addressed determinism).
///
/// Symbolic links and other non-file, non-directory entries are skipped (v1 has
/// no symlink object kind).
///
/// # Errors
///
/// [`WorkError::Io`] if the directory cannot be read or a file cannot be read,
/// or [`WorkError::Store`] if an object cannot be written.
pub fn snapshot(repo: &Repository, dir: &Path) -> Result<ObjectId, WorkError> {
    let tree = snapshot_tree(repo, dir)?;
    repo.write_object(&Object::Tree(tree)).map_err(Into::into)
}

/// Build (but do not persist as the root) the [`Tree`] for one directory level,
/// writing all contained blobs and subtrees into the store.
fn snapshot_tree(repo: &Repository, dir: &Path) -> Result<Tree, WorkError> {
    let mut tree = Tree::new();
    let entries = std::fs::read_dir(dir).map_err(|source| WorkError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| WorkError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            // Non-UTF-8 names have no representable tree-entry name; skip them.
            continue;
        };
        if CONTROL_NAMES.contains(&name) {
            continue;
        }
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| WorkError::Io {
            path: path.clone(),
            source,
        })?;
        if file_type.is_dir() {
            let subtree = snapshot_tree(repo, &path)?;
            let id = repo.write_object(&Object::Tree(subtree))?;
            tree.insert(name, EntryKind::Tree, id)
                .map_err(|e| WorkError::Content(format!("invalid entry name {name:?}: {e}")))?;
        } else if file_type.is_file() {
            let bytes = std::fs::read(&path).map_err(|source| WorkError::Io {
                path: path.clone(),
                source,
            })?;
            let id = repo.write_blob(bytes)?;
            tree.insert(name, EntryKind::Blob, id)
                .map_err(|e| WorkError::Content(format!("invalid entry name {name:?}: {e}")))?;
        }
        // else: symlink/device/etc. — skipped (no object kind in v1).
    }
    Ok(tree)
}

/// **Materialize** (checkout) a snapshot tree into a working directory: remove
/// the existing tracked content and write the tree's content back out, so the
/// directory exactly reflects `tree` afterward (§5.1).
///
/// The control directories (`.omoplata`, `.git`) at the top level are never
/// touched. Everything else under `dir` is removed and then re-created from the
/// tree, so files present in `dir` but absent from `tree` are deleted (a true
/// checkout, not a merge). `tree` is the [`ObjectId`] of a stored [`Tree`]
/// object.
///
/// # Errors
///
/// [`WorkError::Store`] if the tree or a referenced object cannot be read,
/// [`WorkError::Content`] if `tree` is not a tree object, or [`WorkError::Io`]
/// on any filesystem failure.
pub fn materialize(repo: &Repository, tree: &ObjectId, dir: &Path) -> Result<(), WorkError> {
    remove_working_content(dir)?;
    write_tree_into(repo, tree, dir)
}

/// Remove every top-level entry of `dir` except the control directories.
fn remove_working_content(dir: &Path) -> Result<(), WorkError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(WorkError::Io {
                path: dir.to_path_buf(),
                source,
            })
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| WorkError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let name = entry.file_name();
        if name.to_str().is_some_and(|n| CONTROL_NAMES.contains(&n)) {
            continue;
        }
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| WorkError::Io {
            path: path.clone(),
            source,
        })?;
        let result = if file_type.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        result.map_err(|source| WorkError::Io { path, source })?;
    }
    Ok(())
}

/// Write the [`Tree`] identified by `tree` into `dir`, creating files and
/// subdirectories.
fn write_tree_into(repo: &Repository, tree: &ObjectId, dir: &Path) -> Result<(), WorkError> {
    let object = repo.read_object(tree)?;
    let Object::Tree(tree) = object else {
        return Err(WorkError::Content(format!("{tree} is not a tree object")));
    };
    std::fs::create_dir_all(dir).map_err(|source| WorkError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in tree.entries() {
        let path = dir.join(&entry.name);
        match entry.kind {
            EntryKind::Blob => {
                let object = repo.read_object(&entry.id)?;
                let Object::Blob(blob) = object else {
                    return Err(WorkError::Content(format!(
                        "entry {} claims blob but stored object is a tree",
                        entry.name
                    )));
                };
                std::fs::write(&path, blob.bytes()).map_err(|source| WorkError::Io {
                    path: path.clone(),
                    source,
                })?;
            }
            EntryKind::Tree => {
                write_tree_into(repo, &entry.id, &path)?;
            }
        }
    }
    Ok(())
}

/// Whether a workspace's working directory has uncommitted changes relative to
/// `expected` — the commit its current-change tip records (`None` if the change
/// has never been committed, in which case the directory is expected to be
/// empty of tracked content).
///
/// Returns the current snapshot's [`CommitId`] alongside the dirty verdict so a
/// caller can report it. A directory is **clean** iff its snapshot equals the
/// expected tip.
///
/// # Errors
///
/// Propagates [`snapshot`]'s errors.
pub fn is_dirty(
    repo: &Repository,
    dir: &Path,
    expected: Option<&CommitId>,
) -> Result<(bool, CommitId), WorkError> {
    let current = CommitId::new(snapshot(repo, dir)?.to_string());
    let expected = match expected {
        Some(tip) => tip.clone(),
        None => {
            // No recorded tip: the clean baseline is the empty tree.
            let empty = repo.write_object(&Object::Tree(Tree::new()))?;
            CommitId::new(empty.to_string())
        }
    };
    Ok((current != expected, current))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn snapshot_is_content_addressed_and_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let wc = dir.path().join("wc");
        write(&wc, "a.txt", "alpha");
        write(&wc, "sub/b.txt", "beta");

        let id1 = snapshot(&repo, &wc).unwrap();
        let id2 = snapshot(&repo, &wc).unwrap();
        assert_eq!(id1, id2, "same content must snapshot to the same tree id");

        // A change to content changes the id.
        write(&wc, "a.txt", "ALPHA");
        let id3 = snapshot(&repo, &wc).unwrap();
        assert_ne!(id1, id3);
    }

    #[test]
    fn snapshot_skips_control_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let wc = dir.path().join("wc");
        write(&wc, "keep.txt", "x");
        // A nested `.omoplata` and `.git` must be ignored.
        write(&wc, ".omoplata/objects/xx", "junk");
        write(&wc, ".git/HEAD", "ref: refs/heads/main");

        let id = snapshot(&repo, &wc).unwrap();
        let Object::Tree(tree) = repo.read_object(&id).unwrap() else {
            panic!("expected tree");
        };
        assert_eq!(tree.entries().len(), 1);
        assert_eq!(tree.entries()[0].name, "keep.txt");
    }

    #[test]
    fn materialize_round_trips_a_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let src = dir.path().join("src");
        write(&src, "top.txt", "top");
        write(&src, "nested/deep.txt", "deep");
        let id = snapshot(&repo, &src).unwrap();

        let dst = dir.path().join("dst");
        materialize(&repo, &id, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("top.txt")).unwrap(), "top");
        assert_eq!(
            std::fs::read_to_string(dst.join("nested/deep.txt")).unwrap(),
            "deep"
        );
        // Re-snapshotting the materialized copy yields the same id.
        assert_eq!(snapshot(&repo, &dst).unwrap(), id);
    }

    #[test]
    fn materialize_removes_files_absent_from_target() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let wc = dir.path().join("wc");
        write(&wc, "first.txt", "one");
        let first = snapshot(&repo, &wc).unwrap();

        write(&wc, "second.txt", "two");
        let _second = snapshot(&repo, &wc).unwrap();
        assert!(wc.join("second.txt").exists());

        // Checking out the first snapshot must delete second.txt.
        materialize(&repo, &first, &wc).unwrap();
        assert!(wc.join("first.txt").exists());
        assert!(!wc.join("second.txt").exists());
    }

    #[test]
    fn registry_add_list_remove_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let path = WorkspaceRegistry::path_in(&repo);

        let mut reg = WorkspaceRegistry::new();
        reg.add("w1", PathBuf::from("/tmp/w1")).unwrap();
        reg.add("w2", PathBuf::from("/tmp/w2")).unwrap();
        assert!(reg.add("w1", PathBuf::from("/tmp/other")).is_err());
        reg.save(&path).unwrap();

        let loaded = WorkspaceRegistry::load(&path).unwrap();
        assert_eq!(loaded, reg);
        assert_eq!(loaded.workspaces().len(), 2);
        assert_eq!(loaded.get("w1").unwrap().change, ChangeId::new("ws/w1"));

        let mut reg = loaded;
        reg.remove("w1").unwrap();
        assert!(reg.get("w1").is_none());
        assert!(reg.remove("w1").is_err());
    }

    #[test]
    fn mutate_locked_persists_registry() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        WorkspaceRegistry::mutate_locked(&repo, |reg| {
            reg.add("w1", PathBuf::from("/tmp/w1"))?;
            Ok(())
        })
        .unwrap();
        let reg = WorkspaceRegistry::load(WorkspaceRegistry::path_in(&repo)).unwrap();
        assert_eq!(reg.workspaces().len(), 1);
    }

    #[test]
    fn is_dirty_detects_uncommitted_change() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let wc = dir.path().join("wc");
        write(&wc, "a.txt", "one");
        let tip = CommitId::new(snapshot(&repo, &wc).unwrap().to_string());

        // Matching the recorded tip: clean.
        let (dirty, _) = is_dirty(&repo, &wc, Some(&tip)).unwrap();
        assert!(!dirty);

        // Edit without committing: dirty.
        write(&wc, "a.txt", "two");
        let (dirty, _) = is_dirty(&repo, &wc, Some(&tip)).unwrap();
        assert!(dirty);
    }
}
