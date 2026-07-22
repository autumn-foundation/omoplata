//! The LCF-style **kernel admission boundary** — the trusted gate that makes
//! "no silent wrong answers" a structural property of the code (design doc §3
//! principle **P1**, §6 invariant **I8**, resting on **I5**).
//!
//! # What the design doc asks for
//!
//! **P1 — Small verified kernel, untrusted proposers (the LCF architecture):**
//!
//! > The trusted computing base is a minimal merge kernel whose soundness is
//! > proven in Verus. All cleverness — per-language structural merge drivers,
//! > diff heuristics, agent arbiters — lives outside the boundary as untrusted
//! > *proposers*. A proposer emits a candidate merge; the kernel *checks* it via
//! > an executable commutation test over the canonical tree representation. A bad
//! > proposer can produce a rejected proposal or a degraded conflict, never a
//! > silently wrong merge. This is the same kernel/tactic separation that keeps
//! > proof assistants sound with arbitrarily wild tactics.
//!
//! **I8 — Kernel admission (no silent wrong answers):**
//!
//! > every merge result the kernel emits either carries a checked commutation
//! > witness or is a Conflict value. There is no third output.
//!
//! **I5 — Commutation soundness:**
//!
//! > if the kernel judges `p ⇄ q`, then
//! > `apply(apply(base,p),q) == apply(apply(base,q),p)`, exactly.
//!
//! **§4 (Tier-2 admission):**
//!
//! > Kernel admission for Tier-2 proposals checks tree equality *and* trivia
//! > conservation (I11).
//!
//! # What this module builds
//!
//! [`Admission`] is the whole of I8 made a type: it has **exactly two variants**,
//! [`Admission::Merged`] (a document carrying a checked [`CommutationWitness`])
//! and [`Admission::Conflict`] (first-class conflict values). The enum having no
//! third variant *is* the "no third output" guarantee — a caller cannot obtain a
//! merge that was not witnessed.
//!
//! * [`admit`] is the kernel deriving a merge **from first principles**: it
//!   independently diffs both sides, runs the executable commutation check, and —
//!   only if the check passes — computes the merged document itself. It never
//!   trusts a proposer.
//! * [`verify_witness`] is the independent checker the trusted side runs to
//!   re-establish, from `base` and the witness alone, that a claimed result is
//!   the real witnessed merge.
//! * [`certify`] is the LCF gate a proposer's output passes through: the kernel
//!   computes its own answer and admits the proposal only if it *matches* that
//!   answer. A buggy or lying driver is downgraded to a conflict, never
//!   rubber-stamped.
//!
//! This is the line-layer realisation of ADR-0002 ("LCF kernel/proposer
//! admission"). The definition-level (tree-sitter) tree-equality and trivia
//! conservation checks of §4/I11 sit above this same boundary in a later
//! milestone; the shape of the gate does not change.
//!
//! # Example
//!
//! ```
//! use omoplata_algebra::{kernel, Admission, Doc};
//!
//! let base = Doc::from_str("a\nb\nc\nd");
//! let left = Doc::from_str("A\nb\nc\nd"); // edits line 0
//! let right = Doc::from_str("a\nb\nc\nD"); // edits line 3
//!
//! // Disjoint edits commute, so the kernel admits with a checkable witness.
//! match kernel::admit(&base, &left, &right) {
//!     Admission::Merged { result, witness } => {
//!         assert_eq!(result, Doc::from_str("A\nb\nc\nD"));
//!         // The trusted side re-checks the witness from base alone (I5/I8).
//!         assert!(kernel::verify_witness(&base, &witness, &result));
//!     }
//!     Admission::Conflict(_) => unreachable!("disjoint edits are admitted"),
//! }
//! ```

use crate::commute::{combine, commute, Commutation};
use crate::doc::Doc;
use crate::merge::{merge3, Conflict};
use crate::patch::{apply, diff, Patch};

/// A checkable certificate that `base → left` (`p`) and `base → right` (`q`)
/// commute.
///
/// A witness is *not* trusted on its face: it is re-checkable from `base` alone
/// by [`verify_witness`]. It carries the two original diffs (not their rebased
/// forms) so the checker can independently confirm they are the real diffs from
/// `base` to each side, that they commute, and that applying them yields the
/// claimed result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommutationWitness {
    /// The diff from `base` to the left side.
    pub p: Patch,
    /// The diff from `base` to the right side.
    pub q: Patch,
}

/// The **only two outputs** the kernel may emit (invariant I8).
///
/// The enum has exactly two variants by design: a [`Merged`](Admission::Merged)
/// document that carries a checked [`CommutationWitness`], or one or more
/// [`Conflict`](Admission::Conflict) values. There is no third variant, so there
/// is no way for the kernel to hand back a merge that was not witnessed — that
/// exhaustiveness *is* the "no silent wrong answers" guarantee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// The merge was admitted: `result` is the canonical merged document and
    /// `witness` re-checks against `base` via [`verify_witness`].
    Merged {
        /// The kernel's own canonical merged document (I5: identical under both
        /// application orders).
        result: Doc,
        /// The checkable commutation certificate for `result`.
        witness: CommutationWitness,
    },
    /// The merge was refused: the sides do not commute (or a proposal disagreed
    /// with the kernel), reported as first-class conflict values (§5.4).
    Conflict(Vec<Conflict>),
}

/// Admit a three-way merge, deriving the answer from first principles.
///
/// The kernel independently computes `p = diff(base, left)` and
/// `q = diff(base, right)` and runs the executable commutation check
/// [`commute`]. If the two are [`Independent`](Commutation::Independent) it
/// computes the canonical merged document **itself** — applying `p` then the
/// rebased `q`, the both-orders-equal result — and returns
/// [`Admission::Merged`] carrying the witness `{p, q}`. Otherwise it returns
/// [`Admission::Conflict`] with the honest conflict values from [`merge3`].
///
/// The kernel never trusts a proposer here: every input is a `Doc`, every diff
/// and every application is recomputed inside the boundary.
///
/// PROOF OBLIGATION (I8 — kernel admission, no third output): the return type
/// has exactly two variants; a merge is emitted only with a witness, everything
/// else is a `Conflict`. Enforced structurally by [`Admission`].
///
/// PROOF OBLIGATION (I5 — commutation soundness): when this returns
/// [`Admission::Merged`], `result` is the value both application orders agree on
/// (guaranteed by [`commute`]'s `Independent` witness; see the algebra's
/// `disjoint_commutes_both_orders` property). Guarded by the kernel tests.
#[must_use]
pub fn admit(base: &Doc, left: &Doc, right: &Doc) -> Admission {
    let p = diff(base, left);
    let q = diff(base, right);

    match commute(&p, &q) {
        Commutation::Independent { q_rebased, .. } => {
            // The kernel computes the merged document itself: apply p, then the
            // rebased q. On a genuine independence witness over the originating
            // base this cannot fail; if it ever did, the honest response is a
            // conflict, never a guessed merge — so there is still no third
            // output.
            match combine(base, &p, &q_rebased) {
                Ok(result) => Admission::Merged {
                    result,
                    witness: CommutationWitness { p, q },
                },
                Err(_) => Admission::Conflict(merge3(base, left, right).conflicts),
            }
        }
        Commutation::Overlap => Admission::Conflict(merge3(base, left, right).conflicts),
    }
}

/// Re-check, from `base` and the witness alone, that `result` is the real
/// witnessed merge. This is the independent checker the trusted side runs.
///
/// It re-establishes every fact a witness claims:
///
/// 1. `w.p` and `w.q` are the **real diffs** from `base` to each side — each is
///    the canonical diff of its own effect (`diff(base, apply(w.p, base)) == w.p`),
///    so a garbled patch that does not correspond to an actual `base → side`
///    diff is rejected;
/// 2. `w.p` and `w.q` **commute** (else there is no witness to check);
/// 3. applying them yields **exactly** `result`.
///
/// Returns `false` on any failure — a bad context, a non-canonical patch, a
/// non-commuting pair, or a `result` that does not match. It never panics.
///
/// PROOF OBLIGATION (I5/I8): a `true` verdict means `result` is reproducible
/// from `base` and the witness by commuting application, and is therefore the
/// order-independent merge (I5) that the admission boundary (I8) is allowed to
/// emit. Tampering with either patch or with `result` flips the verdict to
/// `false` (guarded by `verify_rejects_tampering`).
#[must_use]
pub fn verify_witness(base: &Doc, w: &CommutationWitness, result: &Doc) -> bool {
    // (1) Each patch must be the canonical diff from `base` to the side it
    // reconstructs. Recompute the side, then re-diff it: a real diff round-trips.
    let Ok(left) = apply(&w.p, base) else {
        return false;
    };
    let Ok(right) = apply(&w.q, base) else {
        return false;
    };
    if diff(base, &left) != w.p || diff(base, &right) != w.q {
        return false;
    }

    // (2) The patches must commute; grab the rebased q from the witness.
    let Commutation::Independent { q_rebased, .. } = commute(&w.p, &w.q) else {
        return false;
    };

    // (3) Commuting application must reproduce `result` exactly.
    match combine(base, &w.p, &q_rebased) {
        Ok(merged) => merged == *result,
        Err(_) => false,
    }
}

/// The LCF gate a proposer's candidate merge passes through.
///
/// The kernel computes its **own** [`admit`] over `base`/`left`/`right`:
///
/// * if that is [`Admission::Merged`] and the kernel's `result` equals the
///   proposer's `proposed`, the proposal is admitted with the witness;
/// * if the kernel's witnessed result **disagrees** with `proposed` (a buggy or
///   lying driver), the kernel does *not* rubber-stamp it — it downgrades to a
///   [`Admission::Conflict`] recording the disagreement (base vs. the kernel's
///   result vs. the proposer's claim);
/// * if the kernel's own [`admit`] was already a conflict, that conflict is
///   returned unchanged.
///
/// This is the concrete demonstration that a wrong proposal cannot be admitted:
/// admission is gated on the proposer *matching* an answer the kernel derived
/// independently, never on the proposer's say-so.
///
/// PROOF OBLIGATION (I8/P1 — untrusted proposers): the proposal is never trusted;
/// it is only ever compared against the kernel's own witnessed result, and a
/// mismatch degrades to a conflict. Guarded by `certify_downgrades_bad_proposal`.
#[must_use]
pub fn certify(base: &Doc, left: &Doc, right: &Doc, proposed: &Doc) -> Admission {
    match admit(base, left, right) {
        Admission::Merged { result, witness } => {
            if result == *proposed {
                Admission::Merged { result, witness }
            } else {
                // The proposer's document is not the kernel's witnessed merge.
                // Downgrade honestly: a conflict recording the disagreement,
                // never a rubber stamp.
                Admission::Conflict(vec![Conflict {
                    base: base.lines().to_vec(),
                    left: result.lines().to_vec(),
                    right: proposed.lines().to_vec(),
                }])
            }
        }
        conflict @ Admission::Conflict(_) => conflict,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(lines: &[&str]) -> Doc {
        Doc::from_lines(lines.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn admit_merges_disjoint_edits_with_a_verifiable_witness() {
        let base = doc(&["a", "b", "c", "d"]);
        let left = doc(&["A", "b", "c", "d"]); // edits line 0
        let right = doc(&["a", "b", "c", "D"]); // edits line 3

        match admit(&base, &left, &right) {
            Admission::Merged { result, witness } => {
                // Both edits present in the merged document.
                assert_eq!(result, doc(&["A", "b", "c", "D"]));
                // The witness re-checks independently from base + witness.
                assert!(verify_witness(&base, &witness, &result));
            }
            Admission::Conflict(_) => panic!("disjoint edits must be admitted"),
        }
    }

    #[test]
    fn admit_conflicts_on_overlapping_edits_never_merges() {
        let base = doc(&["a", "b", "c"]);
        let left = doc(&["a", "X", "c"]); // same line, different value
        let right = doc(&["a", "Y", "c"]);

        match admit(&base, &left, &right) {
            Admission::Conflict(conflicts) => {
                assert_eq!(conflicts.len(), 1);
                assert_eq!(
                    conflicts[0],
                    Conflict {
                        base: vec!["b".to_owned()],
                        left: vec!["X".to_owned()],
                        right: vec!["Y".to_owned()],
                    }
                );
            }
            Admission::Merged { .. } => {
                panic!("overlapping edits must never be admitted as Merged")
            }
        }
    }

    #[test]
    fn certify_admits_the_correct_kernel_result() {
        let base = doc(&["a", "b", "c", "d"]);
        let left = doc(&["A", "b", "c", "d"]);
        let right = doc(&["a", "b", "c", "D"]);

        // The proposer proposes exactly the kernel's canonical result.
        let proposed = doc(&["A", "b", "c", "D"]);
        match certify(&base, &left, &right, &proposed) {
            Admission::Merged { result, witness } => {
                assert_eq!(result, proposed);
                assert!(verify_witness(&base, &witness, &result));
            }
            Admission::Conflict(_) => panic!("a matching proposal must be admitted"),
        }
    }

    #[test]
    fn certify_downgrades_bad_proposal() {
        let base = doc(&["a", "b", "c", "d"]);
        let left = doc(&["A", "b", "c", "d"]);
        let right = doc(&["a", "b", "c", "D"]);

        // A tampered/lying proposal that differs from the kernel's real result.
        // A valid witness exists for the *real* merge, but the kernel must not
        // rubber-stamp this wrong document.
        let tampered = doc(&["A", "b", "c", "EVIL"]);
        match certify(&base, &left, &right, &tampered) {
            Admission::Conflict(conflicts) => {
                assert!(!conflicts.is_empty(), "downgrade must carry a conflict");
            }
            Admission::Merged { .. } => panic!("a wrong proposal must not be admitted"),
        }
    }

    #[test]
    fn certify_passes_through_a_genuine_conflict() {
        let base = doc(&["a", "b", "c"]);
        let left = doc(&["a", "X", "c"]);
        let right = doc(&["a", "Y", "c"]);
        // Even a proposal that happens to equal one side cannot rescue a genuine
        // overlap: admit is already Conflict, so certify returns it.
        let proposed = doc(&["a", "X", "c"]);
        assert!(matches!(
            certify(&base, &left, &right, &proposed),
            Admission::Conflict(_)
        ));
    }

    #[test]
    fn verify_rejects_tampering() {
        let base = doc(&["a", "b", "c", "d"]);
        let left = doc(&["A", "b", "c", "d"]);
        let right = doc(&["a", "b", "c", "D"]);

        let Admission::Merged { result, witness } = admit(&base, &left, &right) else {
            panic!("expected a merge");
        };
        // Sanity: the untouched witness verifies.
        assert!(verify_witness(&base, &witness, &result));

        // (a) Garble a witness patch: replace p with a diff to a different doc.
        let garbled = CommutationWitness {
            p: diff(&base, &doc(&["Z", "b", "c", "d"])),
            q: witness.q.clone(),
        };
        assert!(
            !verify_witness(&base, &garbled, &result),
            "a garbled patch must fail verification against the real result"
        );

        // (b) Alter the result while keeping the real witness.
        let altered = doc(&["A", "b", "c", "X"]);
        assert!(
            !verify_witness(&base, &witness, &altered),
            "an altered result must fail verification"
        );
    }
}
