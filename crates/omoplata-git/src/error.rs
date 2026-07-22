//! Error type for the git interop crate.

use std::path::PathBuf;

/// Errors from git object decoding, loose-object I/O, the round-trip gate, and
/// import into the omoplata store.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// A git object's serialized form is malformed.
    #[error("git object decode error: {0}")]
    Decode(&'static str),
    /// The round-trip gate (I9) failed: a decoded object does not re-encode to
    /// the exact input bytes.
    #[error("round-trip gate failed: object {0} does not re-encode byte-identically")]
    Roundtrip(String),
    /// A loose object's content hashes to a different oid than its path claims.
    #[error("oid mismatch: path claims {expected} but content hashes to {got}")]
    OidMismatch {
        /// The oid derived from the on-disk path.
        expected: String,
        /// The oid recomputed from the object's content.
        got: String,
    },
    /// A loose-object path was not the expected `<xx>/<38 hex>` shape.
    #[error("not a loose-object path: {0}")]
    BadLoosePath(PathBuf),
    /// A git tree entry uses a mode that has no omoplata mapping.
    #[error("unsupported git tree entry mode: {0}")]
    UnsupportedMode(String),
    /// A tree referenced an object that is not present among the loose objects.
    #[error("referenced git object not present: {0}")]
    MissingObject(String),
    /// A packfile or its index (`*.pack` / `*.idx`) is malformed: bad magic,
    /// unsupported version, a truncated stream, a delta that cannot be applied,
    /// or a reconstructed object whose oid does not match the index.
    #[error("malformed git packfile: {0}")]
    Pack(&'static str),
    /// A ref file or `packed-refs` entry could not be read or parsed.
    #[error("malformed git ref {name}: {reason}")]
    BadRef {
        /// The ref name (or path) being read.
        name: String,
        /// Why it could not be parsed.
        reason: &'static str,
    },
    /// A malformed 40-hex git oid string.
    #[error("malformed git oid: {0}")]
    MalformedOid(String),
    /// zlib inflate/deflate failed.
    #[error("zlib error: {0}")]
    Zlib(String),
    /// A filesystem operation failed.
    #[error("i/o error at {path}: {source}")]
    Io {
        /// The path being operated on.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// An error writing into the omoplata store.
    #[error(transparent)]
    Store(#[from] omoplata_store::StoreError),
    /// An error building an omoplata object (e.g. an invalid tree entry name).
    #[error(transparent)]
    Object(#[from] omoplata_store::ObjectError),
}
