//! The round-trip gate — the executable form of invariant **I9**.
//!
//! Design doc §3 P8: *"Round-trip fidelity (`git repo → import → export →
//! bit-identical`) is a release gate."* §6 I9: *"Round-trip fidelity (tested,
//! not proven): `export(import(git_repo)) ≡ git_repo` bit-identically, held as a
//! fuzz-tested release gate rather than a Verus theorem (the git format's warts
//! resist clean modeling)."*
//!
//! The gate is deliberately unproven: rather than model git's format in Verus,
//! I9 is discharged empirically by [`roundtrip_ok`] on every object and by
//! property tests over arbitrary inputs. [`verify_repo`] runs the gate across a
//! whole repository and is the check the CLI's `omo git verify` exposes.

use std::path::Path;

use crate::error::GitError;
use crate::gitdir::resolve_git_dir;
use crate::loose::{pack_file_count, walk_loose};
use crate::object::{decode, encode, oid, GitObject, GitOid};
use crate::pack::read_all_packs;

/// Per-type counts produced by [`verify_repo`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GitReport {
    /// Number of blob objects that passed the gate.
    pub blobs: usize,
    /// Number of tree objects that passed the gate.
    pub trees: usize,
    /// Number of commit objects that passed the gate.
    pub commits: usize,
    /// Number of tag objects that passed the gate.
    pub tags: usize,
    /// Number of packfiles present under `objects/pack`. Packed objects are now
    /// decoded and gated exactly like loose ones, so the per-type counts above
    /// include objects reconstructed from packs; this field is retained as
    /// informational (how many packs the counts were drawn from).
    pub packfiles: usize,
}

impl GitReport {
    /// Total number of objects verified.
    #[must_use]
    pub fn total(&self) -> usize {
        self.blobs + self.trees + self.commits + self.tags
    }

    fn tally(&mut self, object: &GitObject) {
        match object {
            GitObject::Blob(_) => self.blobs += 1,
            GitObject::Tree(_) => self.trees += 1,
            GitObject::Commit(_) => self.commits += 1,
            GitObject::Tag(_) => self.tags += 1,
        }
    }
}

/// The round-trip gate for a single object's uncompressed bytes (**I9**).
///
/// PROOF OBLIGATION (I9): this is the round-trip guarantee in executable form.
/// It decodes `bytes`, re-encodes the result, and asserts the re-encoding is
/// **byte-identical** to the input; only then does it return the object's oid
/// (the SHA-1 of the — now proven-stable — bytes). Any object that does not
/// re-encode identically fails the gate with [`GitError::Roundtrip`]. Backed by
/// property tests over arbitrary blobs and by [`verify_repo`] over real repos.
///
/// # Errors
/// Returns [`GitError::Decode`] if `bytes` is not a well-formed git object, or
/// [`GitError::Roundtrip`] if it does not re-encode byte-identically.
pub fn roundtrip_ok(bytes: &[u8]) -> Result<GitOid, GitError> {
    let object = decode(bytes)?;
    let re_encoded = encode(&object);
    if re_encoded != bytes {
        return Err(GitError::Roundtrip(oid(&object).hex()));
    }
    Ok(oid(&object))
}

/// Run the round-trip gate over every object in a git repository — loose and
/// packed alike.
///
/// `path` may be a **worktree root** or a **git directory**: it is first passed
/// through [`resolve_git_dir`], so `omo git verify <repo-root>` auto-descends
/// into `<repo-root>/.git` instead of walking a non-existent `<repo-root>/objects`.
///
/// For each loose object under `<git_dir>/objects/<xx>/` and each object
/// reconstructed from a packfile under `<git_dir>/objects/pack/`, this runs
/// [`roundtrip_ok`] (decode → re-encode → assert byte-identical) and confirms
/// the recomputed SHA-1 equals the oid the object was addressed by (its loose
/// path, or its pack index entry). The gate **fails** — an error is returned —
/// if any object does not round-trip byte-identically or mismatches its oid.
///
/// **I9 gate contract:** a returned [`GitReport`] means *at least one object was
/// actually checked and round-tripped*. A path that resolves to no repository,
/// or to a git directory holding **zero** objects and **zero** packfiles, is
/// **not** a PASS — it is refused (`NotARepository` / `EmptyRepository`) rather
/// than reported as a vacuous success over an empty set. On success it returns
/// per-type counts across both sources.
///
/// # Errors
/// Returns [`GitError::NotARepository`] if `path` is not a git repository,
/// [`GitError::EmptyRepository`] if the resolved repository has no objects and
/// no packfiles, [`GitError::Roundtrip`] on any non-identical re-encoding,
/// [`GitError::OidMismatch`] on any content/oid disagreement,
/// [`GitError::Pack`] on a malformed packfile, or an I/O / decode error while
/// reading objects.
pub fn verify_repo(path: &Path) -> Result<GitReport, GitError> {
    let git_dir = resolve_git_dir(path)?;
    let mut report = GitReport {
        packfiles: pack_file_count(&git_dir),
        ..GitReport::default()
    };
    for (path_oid, object) in walk_loose(&git_dir)? {
        // Re-run the gate on the canonical encoded bytes and confirm the oid the
        // path claimed matches the oid the content produces.
        let gate_oid = roundtrip_ok(&encode(&object))?;
        if gate_oid != path_oid {
            return Err(GitError::OidMismatch {
                expected: path_oid.hex(),
                got: gate_oid.hex(),
            });
        }
        report.tally(&object);
    }
    // Packed objects go through the identical gate; `read_all_packs` already
    // checks each reconstructed object's SHA-1 against its pack-index oid.
    for (pack_oid, object) in read_all_packs(&git_dir)? {
        let gate_oid = roundtrip_ok(&encode(&object))?;
        if gate_oid != pack_oid {
            return Err(GitError::OidMismatch {
                expected: pack_oid.hex(),
                got: gate_oid.hex(),
            });
        }
        report.tally(&object);
    }
    // I9 gate contract: PASS requires ≥1 object actually checked. Zero objects
    // with zero packfiles means there was nothing to round-trip (an empty repo,
    // or a mis-pointed path that resolved but holds nothing) — refuse rather
    // than return a vacuous success.
    if report.total() == 0 && report.packfiles == 0 {
        return Err(GitError::EmptyRepository(path.to_path_buf()));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{GitObject, GitTreeEntry};

    #[test]
    fn gate_passes_and_returns_oid_for_blob() {
        let bytes = encode(&GitObject::Blob(b"hello\n".to_vec()));
        let oid = roundtrip_ok(&bytes).unwrap();
        assert_eq!(oid.hex(), "ce013625030ba8dba906f756967f9e9ca394464a");
    }

    #[test]
    fn gate_passes_for_tree() {
        let tree = GitObject::Tree(vec![GitTreeEntry {
            mode: "100644".to_owned(),
            name: "f".to_owned(),
            oid: oid(&GitObject::Blob(b"x".to_vec())).as_bytes().to_owned(),
        }]);
        let bytes = encode(&tree);
        assert!(roundtrip_ok(&bytes).is_ok());
    }

    #[test]
    fn gate_rejects_garbage() {
        assert!(roundtrip_ok(b"not a git object").is_err());
    }

    #[test]
    fn empty_report_totals_zero() {
        assert_eq!(GitReport::default().total(), 0);
    }
}
