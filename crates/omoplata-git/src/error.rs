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
    /// The supplied path is not a git repository: it has neither a `.git`
    /// directory (worktree root) nor the `objects/`+`refs/` layout of a git
    /// directory itself. Returned instead of a vacuous success when `omo git`
    /// is pointed at a non-repository (a linked-worktree `.git` *file* also
    /// lands here — it is not resolved in v1).
    #[error(
        "not a git repository: {0} \
         (expected a worktree containing a `.git` directory, \
         or a git directory with `objects/` and `refs/`)"
    )]
    NotARepository(PathBuf),
    /// The path resolves to a real git directory, but it holds no objects (a
    /// freshly-`init`ed repo with no commits, or a repo with no resolvable
    /// refs). There is nothing to round-trip, so the I9 gate refuses rather
    /// than reporting a vacuous PASS over an empty object set.
    #[error("empty git repository: {0} (no objects found — nothing to round-trip)")]
    EmptyRepository(PathBuf),
    /// zlib inflate/deflate failed.
    #[error("zlib error: {0}")]
    Zlib(String),
    /// A violation of the git wire protocol (pkt-line framing, a malformed ref
    /// advertisement, or an unexpected `upload-pack` response).
    #[error("git wire protocol error: {0}")]
    WireProto(&'static str),
    /// The `git upload-pack` / `git-upload-pack` helper could not be spawned,
    /// exited non-zero, or the transport failed while talking to it.
    #[error("git wire transport error: {0}")]
    Wire(String),
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
