//! The commutation checker — Tier 0 and Tier 1 of the merge pipeline (§4).
//!
//! Design doc §4:
//!
//! > *Tier 0 — Disjoint support.* Each patch carries a *support set* [...] the
//! > check is a set intersection [...] no diff-level work, no cache. At fleet
//! > concurrency this screens out the overwhelming majority of pairs.
//! >
//! > *Tier 1 — Commutation.* Derived patches that provably commute are merged
//! > by the kernel directly. [...] proof-backed.
//!
//! For the line layer, both tiers collapse to one executable test: two patches
//! commute iff every pair of hunks (one from each) is cleanly separable on the
//! base — neither overlapping nor sharing an ambiguous insertion point. When
//! they do, [`commute`] returns each patch rebased into the other's applied
//! coordinate space, so applying them in either order yields the identical
//! document.

use crate::doc::Doc;
use crate::patch::{apply, ApplyError, Hunk, Patch};

/// The verdict of [`commute`].
///
/// There are exactly two outcomes — never a third. This mirrors the design
/// doc's invariant **I8 (kernel admission, "no silent wrong answers")**: a pair
/// either carries a checked commutation witness ([`Independent`], with the
/// rebased patches that make both application orders equal) or is refused to
/// the conflict machinery ([`Overlap`]).
///
/// [`Independent`]: Commutation::Independent
/// [`Overlap`]: Commutation::Overlap
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Commutation {
    /// The patches commute. `p_rebased` is `p` with its hunks moved into the
    /// coordinate space left by applying `q`, and vice-versa, so that
    /// `apply(q_rebased, apply(p, base)) == apply(p_rebased, apply(q, base))`.
    Independent {
        /// `p` rebased to apply on top of `q`.
        p_rebased: Patch,
        /// `q` rebased to apply on top of `p`.
        q_rebased: Patch,
    },
    /// The patches touch overlapping or ambiguous base regions and do not
    /// commute; the merge must treat this as a conflict.
    Overlap,
}

/// Whether two hunks from different patches fail to be cleanly separable.
///
/// With `before = h.end <= k.start` and `after = k.end <= h.start` (half-open
/// support intervals), the two hunks are cleanly ordered iff exactly one holds.
/// They conflict iff `before == after`, which captures both failure modes:
///
/// * `!before && !after` — their support intervals genuinely overlap;
/// * `before && after` — only possible when all four endpoints coincide, i.e.
///   two pure insertions anchored at the same base point, whose relative order
///   is ambiguous.
///
/// This predicate is symmetric in its arguments (swapping `h` and `k` swaps
/// `before` and `after`), which is what makes [`commute`] symmetric.
fn hunks_conflict(h: &Hunk, k: &Hunk) -> bool {
    let before = h.base_start + h.remove.len() <= k.base_start;
    let after = k.base_start + k.remove.len() <= h.base_start;
    before == after
}

/// Rebase `p` so it applies on top of an already-applied `other`.
///
/// Each hunk's `base_start` is shifted by `other`'s net line-delta accumulated
/// strictly before that hunk. Because [`commute`] only calls this once the
/// patches are known cleanly separable, every hunk of `other` lies wholly
/// before or wholly after each hunk of `p`, so the shift is well defined.
fn rebase(p: &Patch, other: &Patch) -> Patch {
    let hunks = p
        .hunks()
        .iter()
        .map(|h| {
            let shift = other.delta_before(h.base_start);
            // shift is bounded by the base length, so the cast and add are safe.
            let new_start = (h.base_start as isize + shift).max(0) as usize;
            Hunk {
                base_start: new_start,
                remove: h.remove.clone(),
                insert: h.insert.clone(),
            }
        })
        .collect();
    Patch::from_hunks(hunks)
}

/// Decide whether patches `p` and `q` (against a common base) commute.
///
/// PROOF OBLIGATION (I10 — disjoint-support commutation): patches whose support
/// sets are disjoint commute. Realised as Tier 0: if no hunk of `p` conflicts
/// (per [`hunks_conflict`]) with any hunk of `q`, the two are independent and
/// each is rebased past the other. Guarded by `disjoint_commutes_both_orders`.
///
/// PROOF OBLIGATION (I5 — commutation soundness): if this returns
/// [`Commutation::Independent`], then
/// `apply(apply(base, p), q_rebased) == apply(apply(base, q), p_rebased)`
/// exactly (and both equal the fully-merged document). Guarded by
/// `disjoint_commutes_both_orders`.
///
/// PROOF OBLIGATION (I8 — admission, no third output): the result is either a
/// witnessed [`Commutation::Independent`] or an [`Commutation::Overlap`]; there
/// is no silent middle. Enforced structurally by the two-variant enum.
///
/// The check is symmetric: `commute(p, q)` is `Independent` iff `commute(q, p)`
/// is (guarded by `commute_is_symmetric`).
#[must_use]
pub fn commute(p: &Patch, q: &Patch) -> Commutation {
    let disjoint = p
        .hunks()
        .iter()
        .all(|h| q.hunks().iter().all(|k| !hunks_conflict(h, k)));

    if disjoint {
        Commutation::Independent {
            p_rebased: rebase(p, q),
            q_rebased: rebase(q, p),
        }
    } else {
        Commutation::Overlap
    }
}

/// Apply two commuting patches to a base in the canonical (both-orders-equal)
/// way, given the [`Commutation::Independent`] witness from [`commute`].
///
/// This is a convenience for callers that have already established independence
/// and simply want the merged document. It applies `p` then the rebased `q`.
///
/// # Errors
/// Propagates any [`ApplyError`] from the two applications (none is expected for
/// a genuine independence witness on the originating base).
pub fn combine(base: &Doc, p: &Patch, q_rebased: &Patch) -> Result<Doc, ApplyError> {
    let after_p = apply(p, base)?;
    apply(q_rebased, &after_p)
}
