//! Error type for the identity layer.

/// Errors raised by the change graph and the definition graph.
///
/// Every fallible operation in this crate returns [`IdentityError`]; there is no
/// `unwrap`/`expect`/`panic` in non-test code, so a caller can always recover.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IdentityError {
    /// A change id was referenced that is not present in the graph.
    #[error("unknown change: {0}")]
    UnknownChange(String),

    /// A commit id was referenced that is not registered as a revision.
    #[error("unknown commit: {0}")]
    UnknownCommit(String),

    /// A supersession edge would introduce a cycle, violating invariant I6.
    #[error("supersession {0} -> {1} would create a cycle (violates I6 acyclicity)")]
    SupersessionCycle(String, String),

    /// A commit belonging to a [`Public`](crate::Phase::Public) change may not be
    /// superseded (P5: public changes are immutable).
    #[error("cannot supersede commit {0}: it belongs to a public (immutable) change")]
    PublicImmutable(String),

    /// A phase transition tried to move `Public -> Draft`; phases are monotone.
    #[error("phase for change {0} cannot regress from Public to Draft")]
    PhaseRegression(String),

    /// The tree-sitter Rust grammar could not be loaded.
    #[error("failed to load the tree-sitter Rust grammar: {0}")]
    Grammar(String),

    /// The source text could not be parsed into a syntax tree.
    #[error("failed to parse source into a syntax tree")]
    Parse,

    /// A definition occurrence id was referenced that the graph does not know.
    #[error("unknown definition occurrence: {0}")]
    UnknownOccurrence(u64),
}
