//! omoplata's two-tier identity layer (design doc §7 crate #3,
//! `omoplata-identity`).
//!
//! The design doc pins version identity to two graphs (§3, principles **P5** and
//! **P6**; §5.3 and §5.5):
//!
//! * The **change graph** ([`change`]) — commits are content-addressed and
//!   immutable, while *changes* carry stable ids that survive rebase and amend,
//!   linked by **supersession** edges (Mercurial obsolescence, done properly).
//!   **Phases** (draft/public) formalize what is safe to rewrite (**P5**,
//!   §5.3). The relation is a DAG with no orphaned obsolescence — invariant
//!   **I6**.
//! * The **definition graph** ([`definition`]) — definitions (functions, types,
//!   modules …) are extracted per-language via tree-sitter, receive a stable id
//!   at first commit, and have that identity propagated across versions by a
//!   tiered matcher. A definition is "a durable node with its own history,
//!   independent of file and line" (**P6**, §5.5). Mis-matches are correctable
//!   via first-class [`sever`](definition::DefinitionGraph::sever) /
//!   [`join`](definition::DefinitionGraph::join).
//!
//! # Invariant I6
//!
//! > **I6 Supersession well-formedness:** the change graph is acyclic with no
//! > orphaned obsolescence.
//!
//! I6 is discharged in [`change::ChangeGraph::supersede`]: cycles are rejected
//! (never panicked), and both endpoints of every edge must be registered
//! revisions (no orphans). The obligations are annotated with
//! `// PROOF OBLIGATION (I6): …` at the relevant sites.
//!
//! # Verification status
//!
//! As in `omoplata-algebra`, Verus is not available in this environment, so each
//! I6 obligation is encoded as a `PROOF OBLIGATION (I6)` comment plus an
//! executable unit test. No `unwrap`/`expect`/`panic` appears in non-test code.

mod change;
mod definition;
mod error;

pub use change::{Change, ChangeGraph, ChangeId, CommitId, Phase};
pub use definition::{
    extract_definitions, match_definitions, DefMatch, Definition, DefinitionGraph, DefinitionId,
    DefinitionKind, MatchStatus, OccurrenceId,
};
pub use error::IdentityError;
