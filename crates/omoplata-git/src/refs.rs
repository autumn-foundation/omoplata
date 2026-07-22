//! Reading a git repository's refs: `HEAD`, loose refs under `refs/`, and
//! `packed-refs`.
//!
//! [`read_refs`] returns a deterministic (name-sorted) list of
//! `(refname, GitOid)` pairs — the starting points for the commit-graph walk
//! (M10). It resolves symbolic refs (`ref: refs/heads/main`) to the oid they
//! ultimately name, so `HEAD` on a normal repository resolves to its branch
//! tip. Peeled `packed-refs` lines (`^<oid>`, the commit an annotated tag points
//! at) are skipped: the tag object itself is the ref target and the walk follows
//! its `object` field to the commit.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::GitError;
use crate::object::GitOid;

/// Read every ref of the git repository at `git_dir`.
///
/// Reads `HEAD`, every loose ref under `refs/`, and every non-peeled entry in
/// `packed-refs`, resolving symbolic refs to the oid they name. The result is
/// sorted by ref name for determinism. A ref that cannot be resolved to an oid
/// (a dangling symref) is skipped rather than erroring, so a freshly-`init`ed
/// repository with an unborn `HEAD` yields an empty list.
///
/// # Errors
/// Returns [`GitError::Io`] if a ref file or `packed-refs` cannot be read, or
/// [`GitError::BadRef`] if a ref's contents are not a 40-hex oid or a
/// `ref: <target>` symref.
pub fn read_refs(git_dir: &Path) -> Result<Vec<(String, GitOid)>, GitError> {
    // Collect raw ref contents (oid or symref target) into a name-keyed map.
    let mut raw: BTreeMap<String, RawRef> = BTreeMap::new();

    // packed-refs first, so loose refs (which win in git) can override.
    read_packed_refs(git_dir, &mut raw)?;
    // Loose refs under refs/.
    let refs_root = git_dir.join("refs");
    read_loose_refs(&refs_root, &refs_root, &mut raw)?;
    // HEAD.
    read_head(git_dir, &mut raw)?;

    // Resolve symrefs to oids and drop anything that does not resolve.
    let mut out = Vec::new();
    for name in raw.keys() {
        if let Some(oid) = resolve(name, &raw, 0)? {
            out.push((name.clone(), oid));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// A ref's on-disk value before symref resolution.
#[derive(Debug, Clone)]
enum RawRef {
    /// A direct oid.
    Oid(GitOid),
    /// A symbolic ref naming another ref.
    Symbolic(String),
}

/// Resolve a (possibly symbolic) ref to an oid, following at most a few hops to
/// avoid cycles. Returns `Ok(None)` for a symref that points nowhere resolvable.
fn resolve(
    name: &str,
    raw: &BTreeMap<String, RawRef>,
    depth: usize,
) -> Result<Option<GitOid>, GitError> {
    if depth > 8 {
        return Err(GitError::BadRef {
            name: name.to_owned(),
            reason: "symbolic ref chain too deep (possible cycle)",
        });
    }
    match raw.get(name) {
        Some(RawRef::Oid(oid)) => Ok(Some(*oid)),
        Some(RawRef::Symbolic(target)) => resolve(target, raw, depth + 1),
        None => Ok(None),
    }
}

/// Parse a ref file/entry's textual content into a [`RawRef`].
fn parse_ref_content(name: &str, content: &str) -> Result<RawRef, GitError> {
    let content = content.trim();
    if let Some(target) = content.strip_prefix("ref:") {
        return Ok(RawRef::Symbolic(target.trim().to_owned()));
    }
    GitOid::from_hex(content)
        .map(RawRef::Oid)
        .map_err(|_| GitError::BadRef {
            name: name.to_owned(),
            reason: "not a 40-hex oid or a `ref:` symref",
        })
}

/// Read `HEAD` into the map under the name `"HEAD"`.
fn read_head(git_dir: &Path, raw: &mut BTreeMap<String, RawRef>) -> Result<(), GitError> {
    let head = git_dir.join("HEAD");
    match std::fs::read_to_string(&head) {
        Ok(content) => {
            raw.insert("HEAD".to_owned(), parse_ref_content("HEAD", &content)?);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(GitError::Io { path: head, source }),
    }
}

/// Recursively read loose refs under `dir`, keying them by their path relative
/// to `root` (e.g. `refs/heads/main`).
fn read_loose_refs(
    root: &Path,
    dir: &Path,
    raw: &mut BTreeMap<String, RawRef>,
) -> Result<(), GitError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(GitError::Io {
                path: dir.to_path_buf(),
                source,
            })
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| GitError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| GitError::Io {
            path: path.clone(),
            source,
        })?;
        if file_type.is_dir() {
            read_loose_refs(root, &path, raw)?;
            continue;
        }
        let content = std::fs::read_to_string(&path).map_err(|source| GitError::Io {
            path: path.clone(),
            source,
        })?;
        // Ref name is the path relative to the git dir: `refs/...`. Build it from
        // the `refs/` root plus the relative components, using forward slashes.
        let rel = path.strip_prefix(root).map_err(|_| GitError::BadRef {
            name: path.display().to_string(),
            reason: "ref path is not under refs/",
        })?;
        let mut name = String::from("refs");
        for comp in rel.components() {
            name.push('/');
            name.push_str(&comp.as_os_str().to_string_lossy());
        }
        raw.insert(name.clone(), parse_ref_content(&name, &content)?);
    }
    Ok(())
}

/// Parse `packed-refs`, inserting each `<oid> <refname>` line into the map and
/// skipping comment (`#`) and peeled (`^<oid>`) lines.
fn read_packed_refs(git_dir: &Path, raw: &mut BTreeMap<String, RawRef>) -> Result<(), GitError> {
    let path = git_dir.join("packed-refs");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(GitError::Io { path, source }),
    };
    for line in content.lines() {
        // Comments, the header line, and peeled-oid lines are not ref bindings.
        if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        let Some((oid_s, name)) = line.split_once(' ') else {
            return Err(GitError::BadRef {
                name: line.to_owned(),
                reason: "packed-refs line is not `<oid> <refname>`",
            });
        };
        let name = name.trim();
        let oid = GitOid::from_hex(oid_s.trim()).map_err(|_| GitError::BadRef {
            name: name.to_owned(),
            reason: "packed-refs oid is not 40-hex",
        })?;
        raw.insert(name.to_owned(), RawRef::Oid(oid));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_direct_oid() {
        let r = parse_ref_content(
            "refs/heads/main",
            "ce013625030ba8dba906f756967f9e9ca394464a\n",
        )
        .unwrap();
        match r {
            RawRef::Oid(o) => assert_eq!(o.hex(), "ce013625030ba8dba906f756967f9e9ca394464a"),
            RawRef::Symbolic(_) => panic!("expected oid"),
        }
    }

    #[test]
    fn parse_symbolic() {
        let r = parse_ref_content("HEAD", "ref: refs/heads/main\n").unwrap();
        match r {
            RawRef::Symbolic(t) => assert_eq!(t, "refs/heads/main"),
            RawRef::Oid(_) => panic!("expected symref"),
        }
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_ref_content("x", "not-a-ref").is_err());
    }

    #[test]
    fn resolves_head_symref_through_map() {
        let mut raw = BTreeMap::new();
        let oid = GitOid::from_hex("ce013625030ba8dba906f756967f9e9ca394464a").unwrap();
        raw.insert(
            "HEAD".to_owned(),
            RawRef::Symbolic("refs/heads/main".to_owned()),
        );
        raw.insert("refs/heads/main".to_owned(), RawRef::Oid(oid));
        assert_eq!(resolve("HEAD", &raw, 0).unwrap(), Some(oid));
    }

    #[test]
    fn unresolvable_symref_yields_none() {
        let mut raw = BTreeMap::new();
        raw.insert(
            "HEAD".to_owned(),
            RawRef::Symbolic("refs/heads/unborn".to_owned()),
        );
        assert_eq!(resolve("HEAD", &raw, 0).unwrap(), None);
    }
}
