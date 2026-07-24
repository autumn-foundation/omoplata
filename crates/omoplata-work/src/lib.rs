//! omoplata's working-model layer (design doc §7 crate #4, `omoplata-work`).
//!
//! This crate is the "glue around the verified core" (§7): it wires the object
//! store and the identity graphs into a mutable repository state, and records
//! every mutation in a **bi-temporal operation log**. Two design-doc surfaces
//! live here:
//!
//! * The **operation log** ([`oplog`]) — §5.6, principle **P4**, invariant
//!   **I7**. The repository's mutable state is a set of refs
//!   (`name -> `[`CommitId`]); every mutation is an [`Operation`] with a
//!   monotonic **transaction time**. Undo is an *inverse operation, not history
//!   erasure* — the log never shrinks and never lies about what was believed.
//!   [`OpLog::refs_at`] answers the transaction-time query at the heart of
//!   Thesis claim 3 ("what did we think the history was … as of last Tuesday").
//!
//! * The **auto-rebase engine** ([`RebaseEngine`], reduction **R4**) — §5.3 +
//!   §5.4 + §5.6. It ties the object store, the verified rebase algebra, the
//!   change graph, and the op log together so a change auto-rebases onto an
//!   advancing base, recording each rebase as an [`OpKind::Rebase`] op
//!   (transaction time) *and* a supersession edge (valid time). Conflicts are
//!   carried as **values**, never blocking. [`RebaseEngine::history`] exposes the
//!   two axes as one jointly-queryable surface.
//!
//! * The **revset** engine ([`revset`]) — §5.8, the `RV` node of the §4
//!   architecture. A small revision-set query language over commits and refs:
//!   `a & b`, `a | b`, `~a`, parentheses, the functions `all()` / `heads()` /
//!   `draft()` / `public()`, bare ref names, and `id:<hex>` literals.
//!
//! # Bi-temporality (Thesis claim 3)
//!
//! > *"History is bi-temporal and queryable. The repository records both what
//! > was true (valid time: the commit graph, supersession of changes) and what
//! > was believed (transaction time: the operation log)."*
//!
//! Valid time is owned by `omoplata-identity` (the change graph); this crate
//! owns the transaction-time axis. [`OpLog::refs_at`] makes "what was believed
//! as of transaction time *t*" a first-class query.
//!
//! # Verification status
//!
//! As elsewhere in the workspace, Verus is not available in this environment,
//! so invariant **I7** ("every operation has an inverse; `undo ∘ op ≡ identity`
//! on repository state") is encoded as `// PROOF OBLIGATION (I7)` comments plus
//! executable unit tests. No `unwrap`/`expect`/`panic` appears in non-test code.

mod autorebase;
mod error;
mod oplog;
mod queue;
mod revset;
mod stack;
mod workspace;

pub use autorebase::{ChangeHistory, RebaseEngine, RebaseOutcome, RebaseRecord, StackItem};
pub use error::WorkError;
pub use oplog::{OpKind, OpLog, Operation};
pub use queue::{
    land_submission, land_submission_in_queue, queue_ref, LandResult, QueueGates, QueuePolicy,
    QueueRegistry,
};
pub use revset::{eval, parse, query, MapContext, RevExpr, RevsetContext};
pub use stack::{absorb, Stack};
pub use workspace::{auto_snapshot, is_dirty, materialize, snapshot, Workspace, WorkspaceRegistry};
