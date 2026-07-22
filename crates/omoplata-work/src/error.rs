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
}
