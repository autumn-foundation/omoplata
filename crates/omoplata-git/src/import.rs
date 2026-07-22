//! Import a git repository into the omoplata object store.
//!
//! [`import_repo`] enforces **I9** first — it runs the round-trip gate
//! ([`crate::gate::verify_repo`]) over every loose object and refuses to import
//! if the gate fails — then maps git blobs and trees into
//! [`omoplata_store::Object`]s, recording a `git oid → omoplata ObjectId` map.
//!
//! ## Fidelity caveat (git ↔ omoplata tree mapping)
//! The omoplata tree model distinguishes only [`EntryKind::Blob`] vs
//! [`EntryKind::Tree`], whereas git carries a full octal mode per entry. Git
//! modes `100644` (regular), `100755` (executable), and `120000` (symlink) all
//! collapse to [`EntryKind::Blob`], so the executable bit and symlink-ness are
//! **not** recoverable from the omoplata tree alone. Exact git export must
//! therefore consult the git-side record, which [`GitImport`] keeps
//! authoritative in [`GitImport::git_objects`] (the decoded [`GitObject`]s keyed
//! by their original oid). The commit graph is not modelled in v1 (§8): commits
//! and tags are counted, and all their reachable blobs/trees are imported (every
//! loose blob/tree is imported), but parent/tree edges are left as future work.

use std::collections::HashMap;
use std::path::Path;

use omoplata_store::{Blob, EntryKind, Object, ObjectId, Repository, Tree};

use crate::error::GitError;
use crate::gate::verify_repo;
use crate::loose::walk_loose;
use crate::object::{GitObject, GitOid};

/// The result of importing a git repository into an omoplata store.
#[derive(Debug, Clone)]
pub struct GitImport {
    /// Map from each git object's oid to the omoplata [`ObjectId`] it was
    /// written as. Blobs and trees are present; commits and tags are not
    /// mapped (they are not stored as omoplata objects in v1).
    pub oid_map: HashMap<GitOid, ObjectId>,
    /// The decoded git objects keyed by oid — the authoritative git-side record
    /// used for exact export (see the module-level fidelity caveat).
    pub git_objects: HashMap<GitOid, GitObject>,
    /// Number of blob objects imported.
    pub blobs: usize,
    /// Number of tree objects imported.
    pub trees: usize,
    /// Number of commit objects seen (counted, not modelled in v1).
    pub commits: usize,
    /// Number of tag objects seen (counted, not modelled in v1).
    pub tags: usize,
}

impl GitImport {
    /// Number of `git oid → omoplata ObjectId` mappings recorded.
    #[must_use]
    pub fn mapping_count(&self) -> usize {
        self.oid_map.len()
    }
}

/// Map a git tree-entry mode to an omoplata [`EntryKind`].
///
/// `40000` (or the zero-padded `040000`) → [`EntryKind::Tree`]; `100644`,
/// `100755`, and `120000` → [`EntryKind::Blob`]. Any other mode (e.g. `160000`
/// gitlinks / submodules) is unsupported in v1.
///
/// # Errors
/// Returns [`GitError::UnsupportedMode`] for any unrecognized mode.
pub fn mode_to_kind(mode: &str) -> Result<EntryKind, GitError> {
    match mode {
        "40000" | "040000" => Ok(EntryKind::Tree),
        "100644" | "100755" | "120000" => Ok(EntryKind::Blob),
        other => Err(GitError::UnsupportedMode(other.to_owned())),
    }
}

/// Import every loose object of the git repo at `git_dir` into `repo`.
///
/// Enforces **I9**: the round-trip gate runs over the whole repository first via
/// [`verify_repo`], and import is refused (an error is returned) if the gate
/// fails. Blobs map to [`Object::Blob`] and trees to [`Object::Tree`]; commits
/// and tags are counted. The returned [`GitImport`] records the
/// `git oid → omoplata ObjectId` map and keeps the git-side objects
/// authoritative for exact export.
///
/// # Errors
/// Returns any [`GitError`] from the gate, from reading loose objects, from an
/// unsupported tree-entry mode, or from writing into the omoplata store.
pub fn import_repo(git_dir: &Path, repo: &Repository) -> Result<GitImport, GitError> {
    // I9 enforcement: refuse to import a repo that does not round-trip.
    verify_repo(git_dir)?;

    // Load every loose object into an oid-keyed map so trees can resolve their
    // children regardless of on-disk walk order.
    let mut git_objects: HashMap<GitOid, GitObject> = HashMap::new();
    for (oid, object) in walk_loose(git_dir)? {
        git_objects.insert(oid, object);
    }

    let mut oid_map: HashMap<GitOid, ObjectId> = HashMap::new();
    let mut blobs = 0usize;
    let mut trees = 0usize;
    let mut commits = 0usize;
    let mut tags = 0usize;

    for (oid, object) in &git_objects {
        match object {
            GitObject::Blob(_) => {
                import_object(*oid, &git_objects, repo, &mut oid_map)?;
                blobs += 1;
            }
            GitObject::Tree(_) => {
                import_object(*oid, &git_objects, repo, &mut oid_map)?;
                trees += 1;
            }
            GitObject::Commit(_) => commits += 1,
            GitObject::Tag(_) => tags += 1,
        }
    }

    Ok(GitImport {
        oid_map,
        git_objects,
        blobs,
        trees,
        commits,
        tags,
    })
}

/// Import a single blob or tree (recursing into subtrees), memoizing the result
/// in `oid_map`, and return its omoplata [`ObjectId`].
fn import_object(
    oid: GitOid,
    git_objects: &HashMap<GitOid, GitObject>,
    repo: &Repository,
    oid_map: &mut HashMap<GitOid, ObjectId>,
) -> Result<ObjectId, GitError> {
    if let Some(id) = oid_map.get(&oid) {
        return Ok(id.clone());
    }
    let object = git_objects
        .get(&oid)
        .ok_or_else(|| GitError::MissingObject(oid.hex()))?;
    let store_id = match object {
        GitObject::Blob(bytes) => repo.write_object(&Object::Blob(Blob::new(bytes.clone())))?,
        GitObject::Tree(entries) => {
            let mut tree = Tree::new();
            for entry in entries {
                let kind = mode_to_kind(&entry.mode)?;
                let child_oid = GitOid::from_bytes(entry.oid);
                let child_id = import_object(child_oid, git_objects, repo, oid_map)?;
                tree.insert(entry.name.clone(), kind, child_id)?;
            }
            repo.write_object(&Object::Tree(tree))?
        }
        // Commits and tags are not stored as omoplata objects in v1.
        GitObject::Commit(_) | GitObject::Tag(_) => {
            return Err(GitError::UnsupportedMode(object.type_str().to_owned()));
        }
    };
    oid_map.insert(oid, store_id.clone());
    Ok(store_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_mapping() {
        assert_eq!(mode_to_kind("40000").unwrap(), EntryKind::Tree);
        assert_eq!(mode_to_kind("040000").unwrap(), EntryKind::Tree);
        assert_eq!(mode_to_kind("100644").unwrap(), EntryKind::Blob);
        assert_eq!(mode_to_kind("100755").unwrap(), EntryKind::Blob);
        assert_eq!(mode_to_kind("120000").unwrap(), EntryKind::Blob);
        assert!(mode_to_kind("160000").is_err());
    }
}
