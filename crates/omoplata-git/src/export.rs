//! Exact-mode export — the outbound half of the I9 round-trip (M10).
//!
//! [`export_repo`] writes every object recorded by an [`GitImport`] back out as
//! a loose git object, reconstructed from the decoded model so it is
//! byte-identical to the source, and writes the refs. [`export_matches_source`]
//! is the repo-level gate: it confirms the exported loose-object set has exactly
//! the same object oids and the same object bytes as the source.
//!
//! ## What "byte-identical" means here
//! A git object's identity is the SHA-1 of its **uncompressed** canonical form
//! `"{type} {len}\0{body}"` — that is the byte string the oid commits to and the
//! byte string [`export_matches_source`] compares. It is *not* the zlib-
//! compressed loose-file bytes: zlib compression is not uniquely determined
//! (compressor, level, and version all vary the bytes), and git itself does not
//! promise identical compressed bytes across versions. Two loose files with the
//! same oid decompress to the same canonical object and are, by construction,
//! the same object. The round-trip guarantee (I9) is therefore discharged at the
//! object level, which is the level the oid — and git's own integrity model —
//! defines.
//!
//! ## Packfile scope
//! Export always writes *loose* objects, but its input is complete: the import
//! it consumes decodes both loose and packed objects (unpacking any
//! `OFS_DELTA`/`REF_DELTA` deltas), so a `git gc`'d source exports its full
//! object set. Re-packing on export is not performed — omoplata writes the
//! canonical loose form, which shares each object's oid with git's packed form.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::GitError;
use crate::import::GitImport;
use crate::loose::write_loose;
use crate::object::{encode, GitObject, GitOid};

/// The result of an exact-mode export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitExport {
    /// The directory the git objects and refs were written under.
    pub out_dir: PathBuf,
    /// Number of loose objects written (blobs, trees, commits, tags).
    pub objects: usize,
    /// Number of refs written.
    pub refs: usize,
}

/// Export every object of `import` as a loose git object under `out_dir`, and
/// write its refs.
///
/// Objects are written to `<out_dir>/objects/<xx>/<38 hex>` — reconstructed from
/// the decoded [`GitObject`] via [`crate::object::encode`], so each is
/// byte-identical to the source object (same oid, same canonical bytes). Refs
/// are written under `<out_dir>` at their ref name (`refs/heads/…`,
/// `refs/tags/…`) as loose ref files, plus `HEAD`. `HEAD` is written in detached
/// form (the resolved oid) because [`GitImport`] carries resolved ref targets.
///
/// # Errors
/// Returns [`GitError::OidMismatch`] if a written object's recomputed oid does
/// not match its recorded oid (an internal-consistency failure), or
/// [`GitError::Io`]/[`GitError::Zlib`] on a write failure.
pub fn export_repo(import: &GitImport, out_dir: &Path) -> Result<GitExport, GitError> {
    let objects_dir = out_dir.join("objects");
    let mut objects = 0usize;
    // Deterministic write order (oid-sorted) for reproducibility.
    let mut ordered: Vec<(&GitOid, &GitObject)> = import.git_objects.iter().collect();
    ordered.sort_by_key(|(oid, _)| oid.hex());
    for (oid, object) in ordered {
        let written = write_loose(&objects_dir, object)?;
        if &written != oid {
            return Err(GitError::OidMismatch {
                expected: oid.hex(),
                got: written.hex(),
            });
        }
        objects += 1;
    }

    let mut refs = 0usize;
    for (name, oid) in &import.refs {
        write_ref(out_dir, name, *oid)?;
        refs += 1;
    }

    Ok(GitExport {
        out_dir: out_dir.to_path_buf(),
        objects,
        refs,
    })
}

/// Write a single ref file `<out_dir>/<name>` containing `oid` (40-hex + `\n`).
fn write_ref(out_dir: &Path, name: &str, oid: GitOid) -> Result<(), GitError> {
    // Ref names are `HEAD` or `refs/...`; treat components as a relative path.
    let mut path = out_dir.to_path_buf();
    for comp in name.split('/') {
        path.push(comp);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| GitError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let contents = format!("{}\n", oid.hex());
    std::fs::write(&path, contents).map_err(|source| GitError::Io { path, source })
}

/// The repo-level round-trip gate: are the exported objects byte-identical to
/// the source's objects?
///
/// Both `git_dir` and `out_dir` are walked for their full object set — loose
/// objects *and* objects reconstructed from packfiles — and the two are equal
/// iff they have exactly the same set of oids and each oid's canonical object
/// bytes ([`crate::object::encode`]) match. Returns `true` on a perfect match.
///
/// Including packed objects makes the gate correct for a `git gc`'d source,
/// whose objects live in packs rather than loose files; `out_dir` is written by
/// [`export_repo`] as loose objects, so its pack set is empty and only its loose
/// objects contribute.
///
/// This compares canonical (uncompressed) object bytes, the level the oid
/// commits to — see the module docs on what "byte-identical" means.
///
/// # Errors
/// Propagates any [`GitError`] from walking or decoding either side's objects.
pub fn export_matches_source(git_dir: &Path, out_dir: &Path) -> Result<bool, GitError> {
    let source = all_object_bytes(git_dir)?;
    let exported = all_object_bytes(out_dir)?;
    Ok(source == exported)
}

/// Build an oid → canonical-bytes map of every object under `git_dir`, both
/// loose and packed.
fn all_object_bytes(git_dir: &Path) -> Result<BTreeMap<String, Vec<u8>>, GitError> {
    let mut map = BTreeMap::new();
    for (oid, object) in crate::loose::walk_loose(git_dir)? {
        map.insert(oid.hex(), encode(&object));
    }
    for (oid, object) in crate::pack::read_all_packs(git_dir)? {
        map.entry(oid.hex()).or_insert_with(|| encode(&object));
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{oid, GitObject};
    use std::collections::HashMap;
    use tempfile::tempdir;

    #[test]
    fn export_writes_objects_and_gate_matches() {
        // Build a tiny import by hand: a single blob, no refs.
        let blob = GitObject::Blob(b"hello\n".to_vec());
        let blob_oid = oid(&blob);
        let mut git_objects = HashMap::new();
        git_objects.insert(blob_oid, blob);
        let import = GitImport {
            oid_map: HashMap::new(),
            git_objects,
            commit_dag: HashMap::new(),
            refs: Vec::new(),
            blobs: 1,
            trees: 0,
            commits: 0,
            tags: 0,
        };

        // "Source" dir with the same loose object, written independently.
        let src = tempdir().unwrap();
        write_loose(
            &src.path().join("objects"),
            &GitObject::Blob(b"hello\n".to_vec()),
        )
        .unwrap();

        let out = tempdir().unwrap();
        let export = export_repo(&import, out.path()).unwrap();
        assert_eq!(export.objects, 1);
        assert_eq!(export.refs, 0);

        assert!(export_matches_source(src.path(), out.path()).unwrap());
    }

    #[test]
    fn gate_detects_a_missing_object() {
        let src = tempdir().unwrap();
        write_loose(
            &src.path().join("objects"),
            &GitObject::Blob(b"a\n".to_vec()),
        )
        .unwrap();
        write_loose(
            &src.path().join("objects"),
            &GitObject::Blob(b"b\n".to_vec()),
        )
        .unwrap();
        let out = tempdir().unwrap();
        write_loose(
            &out.path().join("objects"),
            &GitObject::Blob(b"a\n".to_vec()),
        )
        .unwrap();
        assert!(!export_matches_source(src.path(), out.path()).unwrap());
    }
}
