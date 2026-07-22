//! Git object model and a byte-faithful codec.
//!
//! Git addresses an object by the SHA-1 of its *uncompressed* serialized form
//! `"{type} {len}\0{body}"`, and stores that form zlib-compressed as a loose
//! file. This module models the four git object types and provides
//! [`encode`]/[`decode`]/[`oid`] such that a decoded object re-encodes
//! **byte-identically** — the property the round-trip gate (I9) depends on.

use std::fmt;
use std::str::FromStr;

use sha1::{Digest, Sha1};

use crate::error::GitError;

/// A git object identity: the 20-byte SHA-1 of the encoded object.
///
/// Displayed and parsed as 40 lowercase hex characters, matching the name git
/// uses for the loose-object path `<xx>/<38 hex>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GitOid([u8; 20]);

impl GitOid {
    /// Wrap 20 raw SHA-1 bytes as an oid.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 20]) -> Self {
        GitOid(bytes)
    }

    /// The raw 20 SHA-1 bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// Lowercase 40-character hex form.
    #[must_use]
    pub fn hex(&self) -> String {
        hex_encode(&self.0)
    }

    /// Parse a 40-character lowercase hex string into an oid.
    ///
    /// # Errors
    /// Returns [`GitError::MalformedOid`] if the input is not exactly 40 hex
    /// characters.
    pub fn from_hex(s: &str) -> Result<Self, GitError> {
        let bytes = hex_decode(s).ok_or_else(|| GitError::MalformedOid(s.to_owned()))?;
        let arr: [u8; 20] = bytes
            .try_into()
            .map_err(|_| GitError::MalformedOid(s.to_owned()))?;
        Ok(GitOid(arr))
    }
}

impl fmt::Display for GitOid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

impl FromStr for GitOid {
    type Err = GitError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        GitOid::from_hex(s)
    }
}

/// A single entry in a git tree object.
///
/// Git stores each entry as `"{mode} {name}\0"` followed by the referenced
/// object's 20 raw SHA-1 bytes. The `mode` is kept as the exact ASCII string
/// git wrote (e.g. `"100644"`, `"100755"`, `"120000"`, `"40000"`) so that
/// re-encoding is byte-identical — git does *not* zero-pad the tree mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitTreeEntry {
    /// The octal file mode, exactly as stored (no zero-padding normalization).
    pub mode: String,
    /// The entry name (a single path component).
    pub name: String,
    /// The referenced object's raw SHA-1.
    pub oid: [u8; 20],
}

/// A parsed git object.
///
/// Commit and tag bodies are retained as raw bytes in v1: round-trip fidelity
/// (I9) is what matters, and the full commit graph is future work (§8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitObject {
    /// Opaque file content.
    Blob(Vec<u8>),
    /// A directory: an ordered list of tree entries.
    Tree(Vec<GitTreeEntry>),
    /// A commit, kept as its raw body bytes.
    Commit(Vec<u8>),
    /// An annotated tag, kept as its raw body bytes.
    Tag(Vec<u8>),
}

impl GitObject {
    /// The git type keyword for this object (`blob`, `tree`, `commit`, `tag`).
    #[must_use]
    pub fn type_str(&self) -> &'static str {
        match self {
            GitObject::Blob(_) => "blob",
            GitObject::Tree(_) => "tree",
            GitObject::Commit(_) => "commit",
            GitObject::Tag(_) => "tag",
        }
    }
}

/// Encode an object to git's uncompressed serialized form
/// `"{type} {len}\0{body}"`.
///
/// This is the exact byte sequence git hashes with SHA-1 and, once
/// zlib-compressed, writes as a loose object. Trees re-encode byte-identically:
/// mode strings, entry order, names, and raw oids are all preserved.
#[must_use]
pub fn encode(object: &GitObject) -> Vec<u8> {
    let body = match object {
        GitObject::Blob(b) | GitObject::Commit(b) | GitObject::Tag(b) => b.clone(),
        GitObject::Tree(entries) => encode_tree(entries),
    };
    let ty = object.type_str();
    let len = body.len().to_string();
    let mut out = Vec::with_capacity(ty.len() + 1 + len.len() + 1 + body.len());
    out.extend_from_slice(ty.as_bytes());
    out.push(b' ');
    out.extend_from_slice(len.as_bytes());
    out.push(0);
    out.extend_from_slice(&body);
    out
}

fn encode_tree(entries: &[GitTreeEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        out.extend_from_slice(e.mode.as_bytes());
        out.push(b' ');
        out.extend_from_slice(e.name.as_bytes());
        out.push(0);
        out.extend_from_slice(&e.oid);
    }
    out
}

/// Decode git's uncompressed serialized form into a [`GitObject`].
///
/// # Errors
/// Returns [`GitError::Decode`] if the header, declared length, object type, or
/// tree body is malformed.
pub fn decode(bytes: &[u8]) -> Result<GitObject, GitError> {
    let sp = bytes
        .iter()
        .position(|&b| b == b' ')
        .ok_or(GitError::Decode("missing type/size separator"))?;
    let ty = &bytes[..sp];
    let nul = bytes[sp + 1..]
        .iter()
        .position(|&b| b == 0)
        .map(|i| sp + 1 + i)
        .ok_or(GitError::Decode("missing header NUL"))?;
    let size_s =
        std::str::from_utf8(&bytes[sp + 1..nul]).map_err(|_| GitError::Decode("non-utf8 size"))?;
    let size: usize = size_s.parse().map_err(|_| GitError::Decode("bad size"))?;
    let body = &bytes[nul + 1..];
    if body.len() != size {
        return Err(GitError::Decode("declared size does not match body length"));
    }
    match ty {
        b"blob" => Ok(GitObject::Blob(body.to_vec())),
        b"tree" => Ok(GitObject::Tree(decode_tree(body)?)),
        b"commit" => Ok(GitObject::Commit(body.to_vec())),
        b"tag" => Ok(GitObject::Tag(body.to_vec())),
        _ => Err(GitError::Decode("unknown object type")),
    }
}

fn decode_tree(body: &[u8]) -> Result<Vec<GitTreeEntry>, GitError> {
    let mut entries = Vec::new();
    let mut i = 0usize;
    while i < body.len() {
        let sp = body[i..]
            .iter()
            .position(|&b| b == b' ')
            .map(|p| i + p)
            .ok_or(GitError::Decode("tree entry: missing mode separator"))?;
        let mode = std::str::from_utf8(&body[i..sp])
            .map_err(|_| GitError::Decode("tree entry: non-utf8 mode"))?
            .to_owned();
        let nul = body[sp + 1..]
            .iter()
            .position(|&b| b == 0)
            .map(|p| sp + 1 + p)
            .ok_or(GitError::Decode("tree entry: missing name NUL"))?;
        let name = std::str::from_utf8(&body[sp + 1..nul])
            .map_err(|_| GitError::Decode("tree entry: non-utf8 name"))?
            .to_owned();
        let oid_start = nul + 1;
        let oid_end = oid_start
            .checked_add(20)
            .ok_or(GitError::Decode("tree entry: oid offset overflow"))?;
        if oid_end > body.len() {
            return Err(GitError::Decode("tree entry: truncated oid"));
        }
        let mut oid = [0u8; 20];
        oid.copy_from_slice(&body[oid_start..oid_end]);
        entries.push(GitTreeEntry { mode, name, oid });
        i = oid_end;
    }
    Ok(entries)
}

/// The git oid (SHA-1 of the encoded form) of an object.
#[must_use]
pub fn oid(object: &GitObject) -> GitOid {
    GitOid(sha1_bytes(&encode(object)))
}

/// SHA-1 of arbitrary bytes as a 20-byte array.
pub(crate) fn sha1_bytes(bytes: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut arr = [0u8; 20];
    arr.copy_from_slice(&digest);
    arr
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.is_empty() || !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// git's canonical empty-blob oid.
    const EMPTY_BLOB_OID: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

    #[test]
    fn empty_blob_oid_matches_git() {
        let oid = oid(&GitObject::Blob(Vec::new()));
        assert_eq!(oid.hex(), EMPTY_BLOB_OID);
    }

    #[test]
    fn hello_blob_oid_matches_git() {
        // `printf 'hello\n' | git hash-object --stdin` == this constant.
        let oid = oid(&GitObject::Blob(b"hello\n".to_vec()));
        assert_eq!(oid.hex(), "ce013625030ba8dba906f756967f9e9ca394464a");
    }

    #[test]
    fn blob_encode_has_git_header() {
        let encoded = encode(&GitObject::Blob(b"hello\n".to_vec()));
        assert_eq!(&encoded, b"blob 6\0hello\n");
    }

    #[test]
    fn blob_roundtrip() {
        let obj = GitObject::Blob(b"arbitrary\0bytes\xff".to_vec());
        assert_eq!(decode(&encode(&obj)).unwrap(), obj);
    }

    #[test]
    fn tree_roundtrips_byte_identically_and_preserves_order() {
        // A blob entry (100644) sorted before a subtree entry (40000). Git
        // orders tree entries by name; "data" < "src".
        let blob_oid = oid(&GitObject::Blob(b"x".to_vec())).0;
        let sub_oid = oid(&GitObject::Tree(Vec::new())).0;
        let tree = GitObject::Tree(vec![
            GitTreeEntry {
                mode: "100644".to_owned(),
                name: "data".to_owned(),
                oid: blob_oid,
            },
            GitTreeEntry {
                mode: "40000".to_owned(),
                name: "src".to_owned(),
                oid: sub_oid,
            },
        ]);
        let once = encode(&tree);
        let twice = encode(&decode(&once).unwrap());
        assert_eq!(once, twice, "tree must re-encode byte-identically");

        // Name ordering is preserved through decode.
        let GitObject::Tree(entries) = decode(&once).unwrap() else {
            panic!("expected tree");
        };
        assert_eq!(entries[0].name, "data");
        assert_eq!(entries[1].name, "src");
        assert_eq!(entries[1].mode, "40000");
    }

    #[test]
    fn oid_hex_roundtrips() {
        let o = oid(&GitObject::Blob(b"hello\n".to_vec()));
        assert_eq!(GitOid::from_hex(&o.hex()).unwrap(), o);
        assert_eq!(o.to_string().parse::<GitOid>().unwrap(), o);
    }

    #[test]
    fn from_hex_rejects_bad_input() {
        assert!(GitOid::from_hex("").is_err());
        assert!(GitOid::from_hex("abc").is_err()); // odd length
        assert!(GitOid::from_hex(&"zz".repeat(20)).is_err()); // non-hex
        assert!(GitOid::from_hex(&"ab".repeat(19)).is_err()); // too short
    }

    #[test]
    fn decode_rejects_malformed() {
        assert!(decode(b"blob 99\0hi").is_err()); // length mismatch
        assert!(decode(b"no-space").is_err());
        assert!(decode(b"widget 0\0").is_err()); // unknown type
    }
}
