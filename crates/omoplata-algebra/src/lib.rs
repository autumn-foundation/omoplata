//! omoplata's verified merge core — the line/opaque layer (design doc §7 crate
//! #2, `omoplata-algebra`).
//!
//! This crate implements the design doc's patch algebra (§5.2), the Tier-0/Tier-1
//! commutation checker (§4), and three-way merge with conflicts-as-values
//! (§5.4), over an opaque line-oriented [`Doc`]. It is the "point of the
//! project": the small kernel whose soundness the design doc pins to a handful
//! of invariants — **I1a/I1b** (diff determinism and faithfulness), **I5**
//! (commutation soundness), **I8** (kernel admission: no silent wrong answers),
//! and the enabling lemma **I10** (disjoint-support commutation). **I2** (merge
//! symmetry) holds by construction here; **I4** (conflict confluence) is the
//! elevation target that later milestones own.
//!
//! # Verification status
//!
//! The design doc calls for these invariants to be proven in Verus. Verus is
//! not available in this environment (see
//! `docs/adr/0003-verification-strategy.md`), so each obligation is encoded as a
//! `// PROOF OBLIGATION (Ix): …` comment at the relevant function **and** as an
//! executable `proptest` property. The crate boundary is drawn so a Verus proof
//! can be added later without any API change.
//!
//! # Layers
//!
//! This is the opaque/line layer of §5.2's two-layer diff. The definition-level
//! (tree-sitter) layer is a later milestone; the API here is the one the tree
//! layer will sit beneath.
//!
//! # Example
//!
//! ```
//! use omoplata_algebra::{diff, apply, Doc};
//!
//! let base = Doc::from_str("a\nb\nc");
//! let target = Doc::from_str("a\nB\nc");
//! let patch = diff(&base, &target);
//! // I1b — faithfulness: the diff round-trips exactly.
//! assert_eq!(apply(&patch, &base).unwrap(), target);
//! ```

mod commute;
mod doc;
pub mod kernel;
mod merge;
mod patch;

pub use commute::{combine, commute, Commutation};
pub use doc::Doc;
pub use kernel::{admit, certify, verify_witness, Admission, CommutationWitness};
pub use merge::{merge3, Conflict, Merge, CONFLICT_END, CONFLICT_SEP, CONFLICT_START};
pub use patch::{apply, diff, ApplyError, Hunk, Patch};

#[cfg(test)]
mod tests;
