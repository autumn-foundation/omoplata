//! Loose-object I/O: zlib (de)compression and the `<xx>/<38 hex>` layout.
//!
//! Git stores each object zlib-compressed at `.git/objects/<xx>/<38 hex>`,
//! where the 40-hex filename is the object's SHA-1. This module inflates and
//! decodes such files (verifying the content hashes to the path) and writes
//! objects back out in the same layout.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::error::GitError;
use crate::object::{decode, encode, sha1_bytes, GitObject, GitOid};

/// Inflate zlib-compressed loose-object bytes.
pub(crate) fn inflate(data: &[u8]) -> Result<Vec<u8>, GitError> {
    let mut decoder = ZlibDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| GitError::Zlib(e.to_string()))?;
    Ok(out)
}

/// Deflate bytes to git's zlib loose-object encoding.
pub(crate) fn deflate(data: &[u8]) -> Result<Vec<u8>, GitError> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .map_err(|e| GitError::Zlib(e.to_string()))?;
    encoder.finish().map_err(|e| GitError::Zlib(e.to_string()))
}

/// Recover the oid a loose-object `path` encodes, from `<xx>/<38 hex>`.
///
/// # Errors
/// Returns [`GitError::BadLoosePath`] if the path does not have a 2-hex parent
/// directory and a 38-hex filename.
pub fn oid_from_loose_path(path: &Path) -> Result<GitOid, GitError> {
    let file = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| GitError::BadLoosePath(path.to_path_buf()))?;
    let dir = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .ok_or_else(|| GitError::BadLoosePath(path.to_path_buf()))?;
    if dir.len() != 2 || file.len() != 38 {
        return Err(GitError::BadLoosePath(path.to_path_buf()));
    }
    GitOid::from_hex(&format!("{dir}{file}"))
        .map_err(|_| GitError::BadLoosePath(path.to_path_buf()))
}

/// Read, inflate, and decode a loose object at `path`, verifying that its
/// content hashes to the oid the path encodes.
///
/// # Errors
/// Returns [`GitError::Io`] on read failure, [`GitError::Zlib`] on a corrupt
/// stream, [`GitError::Decode`] on a malformed object, [`GitError::BadLoosePath`]
/// if the path is not a loose-object path, or [`GitError::OidMismatch`] if the
/// content does not hash to the path's oid.
pub fn read_loose(path: &Path) -> Result<(GitOid, GitObject), GitError> {
    let raw = std::fs::read(path).map_err(|source| GitError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let inflated = inflate(&raw)?;
    let object = decode(&inflated)?;
    let computed = GitOid::from_bytes(sha1_bytes(&inflated));
    let expected = oid_from_loose_path(path)?;
    if computed != expected {
        return Err(GitError::OidMismatch {
            expected: expected.hex(),
            got: computed.hex(),
        });
    }
    Ok((computed, object))
}

/// Encode, zlib-compress, and write an object under `objects_dir` at
/// `<xx>/<38 hex>`, returning its oid.
///
/// # Errors
/// Returns [`GitError::Zlib`] on a compression failure or [`GitError::Io`] on a
/// filesystem failure.
pub fn write_loose(objects_dir: &Path, object: &GitObject) -> Result<GitOid, GitError> {
    let encoded = encode(object);
    let oid = GitOid::from_bytes(sha1_bytes(&encoded));
    let hex = oid.hex();
    let (shard, rest) = hex.split_at(2);
    let dir = objects_dir.join(shard);
    std::fs::create_dir_all(&dir).map_err(|source| GitError::Io {
        path: dir.clone(),
        source,
    })?;
    let path = dir.join(rest);
    let compressed = deflate(&encoded)?;
    std::fs::write(&path, compressed).map_err(|source| GitError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(oid)
}

/// Walk every loose object under `<git_dir>/objects/<xx>/`, reading and decoding
/// each. Non-object entries (`pack/`, `info/`, temp files) are skipped.
///
/// Each returned oid is verified against its on-disk path via [`read_loose`].
///
/// # Errors
/// Propagates any [`GitError`] from reading, inflating, or decoding an object.
pub fn walk_loose(git_dir: &Path) -> Result<Vec<(GitOid, GitObject)>, GitError> {
    let objects = git_dir.join("objects");
    let mut out = Vec::new();
    let shards = match std::fs::read_dir(&objects) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(source) => {
            return Err(GitError::Io {
                path: objects,
                source,
            })
        }
    };
    for shard in shards {
        let shard = shard.map_err(|source| GitError::Io {
            path: objects.clone(),
            source,
        })?;
        let name = shard.file_name();
        let Some(name) = name.to_str() else { continue };
        // Only two-hex-character shard directories hold loose objects.
        if name.len() != 2 || !name.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let shard_path = shard.path();
        let files = std::fs::read_dir(&shard_path).map_err(|source| GitError::Io {
            path: shard_path.clone(),
            source,
        })?;
        for file in files {
            let file = file.map_err(|source| GitError::Io {
                path: shard_path.clone(),
                source,
            })?;
            let fname = file.file_name();
            let Some(fname) = fname.to_str() else {
                continue;
            };
            if fname.len() != 38 || !fname.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            out.push(read_loose(&file.path())?);
        }
    }
    Ok(out)
}

/// Count the `*.pack` files under `<git_dir>/objects/pack`.
///
/// Packed objects are decoded and verified alongside loose objects by
/// [`crate::pack::read_all_packs`]. This count is included in [`crate::GitReport`]
/// as informational metadata.
#[must_use]
pub fn pack_file_count(git_dir: &Path) -> usize {
    let pack_dir = git_dir.join("objects").join("pack");
    match std::fs::read_dir(&pack_dir) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "pack"))
            .count(),
        Err(_) => 0,
    }
}

/// The conventional loose-object path for `oid` under `objects_dir`.
#[must_use]
pub fn loose_path(objects_dir: &Path, oid: &GitOid) -> PathBuf {
    let hex = oid.hex();
    let (shard, rest) = hex.split_at(2);
    objects_dir.join(shard).join(rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn write_then_read_loose_roundtrips() {
        let dir = tempdir().unwrap();
        let objects = dir.path().join("objects");
        let obj = GitObject::Blob(b"hello\n".to_vec());
        let oid = write_loose(&objects, &obj).unwrap();
        assert_eq!(oid.hex(), "ce013625030ba8dba906f756967f9e9ca394464a");

        let path = loose_path(&objects, &oid);
        let (read_oid, read_obj) = read_loose(&path).unwrap();
        assert_eq!(read_oid, oid);
        assert_eq!(read_obj, obj);
    }

    #[test]
    fn deflate_inflate_roundtrips() {
        let data = b"blob 6\0hello\n";
        let comp = deflate(data).unwrap();
        assert_eq!(inflate(&comp).unwrap(), data);
    }

    #[test]
    fn oid_from_path_parses_shard_layout() {
        let p = Path::new("/x/objects/ce/013625030ba8dba906f756967f9e9ca394464a");
        let oid = oid_from_loose_path(p).unwrap();
        assert_eq!(oid.hex(), "ce013625030ba8dba906f756967f9e9ca394464a");
    }

    #[test]
    fn oid_from_bad_path_errors() {
        assert!(oid_from_loose_path(Path::new("/x/objects/ce/short")).is_err());
        assert!(oid_from_loose_path(Path::new("/x/pack/whatever")).is_err());
    }
}
