//! Error type for the working-model layer.

/// Errors raised by the operation log and the revset engine.
///
/// Every fallible operation in this crate returns [`WorkError`]; there is no
/// `unwrap`/`expect`/`panic` in non-test code, so a caller can always recover.
#[derive(Debug, thiserror::Error)]
pub enum WorkError {
    /// [`undo`](crate::OpLog::undo) was called but no operation remains to be
    /// undone — every prior operation has already been inverted.
    #[error("nothing to undo: the operation log has no un-undone operation")]
    NothingToUndo,

    /// A revset expression referenced a ref name that does not resolve to a
    /// commit in the current context.
    #[error("unknown ref: {0}")]
    UnknownRef(String),

    /// A revset expression could not be tokenized or parsed.
    #[error("revset parse error: {0}")]
    Parse(String),

    /// The op-log file could not be read or written.
    #[error("operation-log I/O error at {path}: {source}")]
    Io {
        /// The path that was being read or written.
        path: std::path::PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A line of the persisted op log could not be decoded as an [`Operation`].
    ///
    /// [`Operation`]: crate::Operation
    #[error("failed to decode operation log entry: {0}")]
    Decode(String),

    /// The object store failed while loading or storing a change's content
    /// during an auto-rebase.
    #[error(transparent)]
    Store(#[from] omoplata_store::StoreError),

    /// The change graph rejected a supersession or revision update during an
    /// auto-rebase (e.g. a public change, or an orphaned/cyclic edge — I6).
    #[error(transparent)]
    Identity(#[from] omoplata_identity::IdentityError),

    /// A commit's stored content could not be interpreted as a text document:
    /// its id was malformed, the object was not a blob, or its bytes were not
    /// valid UTF-8.
    #[error("invalid change content: {0}")]
    Content(String),

    /// [`add`](crate::WorkspaceRegistry::add) was asked to register a workspace
    /// whose name is already taken.
    #[error("a workspace named {0:?} already exists")]
    WorkspaceExists(String),

    /// A workspace name did not resolve to a registered workspace.
    #[error("unknown workspace: {0:?}")]
    UnknownWorkspace(String),

    /// A change ID was unknown or not found in the stack/graph.
    #[error("unknown change: {0}")]
    UnknownChange(String),

    /// A stack index was out of bounds.
    #[error("invalid stack index: {0}")]
    InvalidStackIndex(usize),

    /// A submission was not approved and cannot be landed.
    #[error("submission {0} is not approved")]
    SubmissionNotApproved(String),

    /// [`add`](crate::QueueRegistry::add) was asked to register a queue whose
    /// name is already taken.
    #[error("a queue named {0:?} already exists")]
    QueueExists(String),

    /// A queue name did not resolve to a registered queue (and was not the
    /// implicit `trunk`).
    #[error("unknown queue: {0:?}")]
    UnknownQueue(String),

    /// The submission's content carries unresolved conflict values and the
    /// target queue's policy refuses them (ADR-0009).
    #[error(
        "queue {queue:?} refuses to land content carrying {count} unresolved \
         conflict value(s); resolve them first or land into a queue with \
         allow_carried"
    )]
    QueueCarriedRefused {
        /// The refusing queue.
        queue: String,
        /// How many conflict values the content carries.
        count: usize,
    },

    /// The target queue's P9 validator did not pass (ADR-0009).
    #[error("queue {queue:?} validation gate failed: {reason}")]
    QueueValidationFailed {
        /// The refusing queue.
        queue: String,
        /// Why the gate failed.
        reason: String,
    },

    /// A [`switch`](crate::materialize) would overwrite uncommitted changes in a
    /// workspace's working directory (a dirty working copy). Commit first, or
    /// pass `--force`.
    #[error(
        "workspace {workspace:?} has uncommitted changes (working copy {current} \
         != tip {expected}); commit first or pass --force"
    )]
    DirtyWorkingCopy {
        /// The workspace whose working copy is dirty.
        workspace: String,
        /// The current working-copy snapshot commit id.
        current: String,
        /// The tip the switch was measured against.
        expected: String,
    },
}
