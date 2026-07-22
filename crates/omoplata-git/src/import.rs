//! Import a git repository into the omoplata object store by walking the commit
//! graph from the repository's refs (M10).
//!
//! [`import_repo`] reads the repo's refs ([`crate::refs::read_refs`]), then walks
//! the commit DAG transitively from every ref — following commit `parents`,
//! commit `tree`s (and their subtrees), and annotated-tag targets — importing
//! every reachable blob and tree into the omoplata store and recording the
//! commit DAG and ref list. **I9 is enforced**: every reachable object is run
//! through [`crate::gate::roundtrip_ok`] (decode → re-encode → assert
//! byte-identical) before it is accepted; an object that does not round-trip
//! refuses the whole import.
//!
//! ## Fidelity caveat (git ↔ omoplata tree mapping)
//! The omoplata tree model distinguishes only [`EntryKind::Blob`] vs
//! [`EntryKind::Tree`], whereas git carries a full octal mode per entry. Git
//! modes `100644` (regular), `100755` (executable), and `120000` (symlink) all
//! collapse to [`EntryKind::Blob`], so the executable bit and symlink-ness are
//! **not** recoverable from the omoplata tree alone. Exact git export therefore
//! consults the git-side record, which [`GitImport`] keeps authoritative in
//! [`GitImport::git_objects`] (the decoded [`GitObject`]s keyed by their original
//! oid) — [`crate::export::export_repo`] reconstructs from those, not from the
//! omoplata trees.
//!
//! ## Packfiles
//! Both loose objects and packed objects (from `objects/pack/*.pack`, including
//! `OFS_DELTA`/`REF_DELTA` deltas) are decoded and merged into one oid-keyed
//! map before the walk, so a `git gc`'d repository imports identically to a
//! loose one. A reachable object absent from both sources is a genuine
//! corruption and fails with [`GitError::MissingObject`].

use std::collections::HashMap;
use std::path::Path;

use omoplata_store::{Blob, EntryKind, Object, ObjectId, Repository, Tree};

use crate::error::GitError;
use crate::gate::verify_repo;
use crate::loose::walk_loose;
use crate::object::{encode, GitCommit, GitObject, GitOid};
use crate::pack::read_all_packs;
use crate::refs::read_refs;

/// The result of importing a git repository into an omoplata store.
#[derive(Debug, Clone)]
pub struct GitImport {
    /// Map from each imported blob/tree git oid to the omoplata [`ObjectId`] it
    /// was written as. Commits and tags are not mapped (they are not stored as
    /// omoplata objects in v1); the commit DAG lives in [`Self::commit_dag`].
    pub oid_map: HashMap<GitOid, ObjectId>,
    /// Every reachable git object keyed by oid — the authoritative git-side
    /// record used for exact export (see the module-level fidelity caveat).
    pub git_objects: HashMap<GitOid, GitObject>,
    /// The commit DAG: each reachable commit's oid mapped to its parsed
    /// [`GitCommit`] (`tree`, `parents`, `author`, `committer`, `message`).
    pub commit_dag: HashMap<GitOid, GitCommit>,
    /// The repository's refs (`HEAD`, branches, tags), name-sorted, as read at
    /// import time — the roots of the walk.
    pub refs: Vec<(String, GitOid)>,
    /// Number of blob objects imported.
    pub blobs: usize,
    /// Number of tree objects imported.
    pub trees: usize,
    /// Number of commit objects reachable and recorded in the DAG.
    pub commits: usize,
    /// Number of annotated-tag objects reachable and recorded.
    pub tags: usize,
}

impl GitImport {
    /// Number of `git oid → omoplata ObjectId` mappings recorded (blobs+trees).
    #[must_use]
    pub fn mapping_count(&self) -> usize {
        self.oid_map.len()
    }

    /// The commit oids of the DAG, newest-first: a reverse-topological order
    /// where every commit precedes its parents. Ties are broken by oid hex for
    /// determinism.
    ///
    /// Roots are the ref-pointed commits (and tag targets); the walk emits a
    /// child before any of its parents.
    #[must_use]
    pub fn commit_log(&self) -> Vec<GitOid> {
        // Kahn-style ordering on child→parent edges: emit a commit only once all
        // commits that list it as a parent have been emitted.
        let mut remaining_children: HashMap<GitOid, usize> = HashMap::new();
        for oid in self.commit_dag.keys() {
            remaining_children.entry(*oid).or_insert(0);
        }
        for commit in self.commit_dag.values() {
            for parent in &commit.parents {
                if self.commit_dag.contains_key(parent) {
                    *remaining_children.entry(*parent).or_insert(0) += 1;
                }
            }
        }
        let mut ready: Vec<GitOid> = remaining_children
            .iter()
            .filter(|(_, n)| **n == 0)
            .map(|(oid, _)| *oid)
            .collect();
        ready.sort_by_key(|o| std::cmp::Reverse(o.hex()));
        let mut out = Vec::with_capacity(self.commit_dag.len());
        while let Some(oid) = ready.pop() {
            out.push(oid);
            if let Some(commit) = self.commit_dag.get(&oid) {
                let mut newly_ready = Vec::new();
                for parent in &commit.parents {
                    if let Some(n) = remaining_children.get_mut(parent) {
                        *n -= 1;
                        if *n == 0 {
                            newly_ready.push(*parent);
                        }
                    }
                }
                newly_ready.sort_by_key(|o| std::cmp::Reverse(o.hex()));
                ready.extend(newly_ready);
            }
        }
        out
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

/// Import the git repo at `git_dir` into `repo` by walking the commit graph from
/// its refs.
///
/// Enforces **I9**: [`verify_repo`] runs the round-trip gate over every loose
/// object first, and each reachable object is re-checked with
/// [`crate::gate::roundtrip_ok`] as it is visited — import is refused (an error
/// is returned) if any object fails. Starting from every ref, the walk follows
/// commit parents, commit trees (and subtrees), and tag targets, importing
/// blobs → [`Object::Blob`] and trees → [`Object::Tree`] and recording the
/// commit DAG and ref list on the returned [`GitImport`].
///
/// # Errors
/// Returns [`GitError::MissingObject`] if a reachable object is absent from both
/// the loose store and the packfiles; [`GitError::Pack`] on a malformed
/// packfile; [`GitError::Roundtrip`] on any object that does not re-encode
/// byte-identically; or any error from the gate, ref reading, an unsupported
/// tree-entry mode, or writing into the omoplata store.
pub fn import_repo(git_dir: &Path, repo: &Repository) -> Result<GitImport, GitError> {
    // I9 enforcement: refuse to import a repo whose objects do not round-trip
    // byte-identically (the gate now covers both loose and packed objects).
    verify_repo(git_dir)?;

    // Load every loose *and* packed object into one oid-keyed map so the walk can
    // resolve children regardless of on-disk representation or order.
    let mut objects: HashMap<GitOid, GitObject> = HashMap::new();
    for (oid, object) in walk_loose(git_dir)? {
        objects.insert(oid, object);
    }
    for (oid, object) in read_all_packs(git_dir)? {
        objects.entry(oid).or_insert(object);
    }

    let refs = read_refs(git_dir)?;

    let mut import = GitImport {
        oid_map: HashMap::new(),
        git_objects: HashMap::new(),
        commit_dag: HashMap::new(),
        refs: refs.clone(),
        blobs: 0,
        trees: 0,
        commits: 0,
        tags: 0,
    };

    // Walk the reachable set from every ref (depth-first over an explicit stack).
    let mut stack: Vec<GitOid> = refs.iter().map(|(_, oid)| *oid).collect();
    while let Some(oid) = stack.pop() {
        if import.git_objects.contains_key(&oid) {
            continue;
        }
        let object = resolve_object(oid, &objects)?;
        // I9: every reachable object must round-trip byte-identically.
        crate::gate::roundtrip_ok(&encode(&object))?;
        match &object {
            GitObject::Commit(commit) => {
                import.commits += 1;
                import.commit_dag.insert(oid, commit.clone());
                stack.push(commit.tree);
                for parent in &commit.parents {
                    stack.push(*parent);
                }
            }
            GitObject::Tag(tag) => {
                import.tags += 1;
                // Follow the tag to its target object (usually a commit).
                stack.push(tag.object);
            }
            GitObject::Tree(entries) => {
                import.trees += 1;
                import_tree_object(oid, &object, &objects, repo, &mut import.oid_map)?;
                for entry in entries {
                    stack.push(GitOid::from_bytes(entry.oid));
                }
            }
            GitObject::Blob(bytes) => {
                import.blobs += 1;
                let id = repo.write_object(&Object::Blob(Blob::new(bytes.clone())))?;
                import.oid_map.insert(oid, id);
            }
        }
        import.git_objects.insert(oid, object);
    }

    Ok(import)
}

/// Resolve `oid` from the merged loose+packed object map, or report it missing.
fn resolve_object(
    oid: GitOid,
    objects: &HashMap<GitOid, GitObject>,
) -> Result<GitObject, GitError> {
    match objects.get(&oid) {
        Some(object) => Ok(object.clone()),
        None => Err(GitError::MissingObject(oid.hex())),
    }
}

/// Import a tree (recursing into subtrees), memoizing results in `oid_map`.
///
/// This mirrors the reachability walk's tree/blob handling but is written
/// recursively so a tree's omoplata [`ObjectId`] can be built from its children's
/// ids (the omoplata tree stores child ids, not git oids).
fn import_tree_object(
    oid: GitOid,
    object: &GitObject,
    objects: &HashMap<GitOid, GitObject>,
    repo: &Repository,
    oid_map: &mut HashMap<GitOid, ObjectId>,
) -> Result<ObjectId, GitError> {
    if let Some(id) = oid_map.get(&oid) {
        return Ok(id.clone());
    }
    let id = match object {
        GitObject::Blob(bytes) => repo.write_object(&Object::Blob(Blob::new(bytes.clone())))?,
        GitObject::Tree(entries) => {
            let mut tree = Tree::new();
            for entry in entries {
                let kind = mode_to_kind(&entry.mode)?;
                let child_oid = GitOid::from_bytes(entry.oid);
                let child_obj = resolve_object(child_oid, objects)?;
                let child_id =
                    import_tree_object(child_oid, &child_obj, objects, repo, oid_map)?;
                tree.insert(entry.name.clone(), kind, child_id)?;
            }
            repo.write_object(&Object::Tree(tree))?
        }
        GitObject::Commit(_) | GitObject::Tag(_) => {
            return Err(GitError::UnsupportedMode(object.type_str().to_owned()));
        }
    };
    oid_map.insert(oid, id.clone());
    Ok(id)
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
