//! **P9 — dynamic validation over static omniscience** (design doc §3 principle
//! **P9**, §4 Tier-2 line "Acceptance is provisional pending dynamic validation
//! (P9)", §4 Tier-3 "semantic conflict", and the per-instance runtime guard
//! **I12**).
//!
//! # What the design doc asks for
//!
//! **P9 — Dynamic validation over static omniscience:**
//!
//! > Behavioral merge correctness is undecidable and per-language static
//! > analysis is a tar pit. Instead: every kernel-accepted merge above Tier 1 is
//! > *provisional* until the merge commit passes build + test in CI. A failed
//! > validation demotes the merge to a semantic conflict carrying both sides'
//! > intent metadata.
//!
//! **§4 (Tier-2 admission):** "Acceptance is provisional pending dynamic
//! validation (P9)." — a kernel admission ([`Admission::Merged`]) is not the
//! final word; it is provisional.
//!
//! **§4 (Tier-3 — Semantic conflict):** "What survives Tiers 1–2, *or fails
//! dynamic validation*, is presented as a semantic conflict: both sides'
//! definition-level intent, provenance … — not `<<<<<<<` soup."
//!
//! **I12 — runtime confluence check (resolution admission):** the runtime guard
//! that "degrades to a fresh conflict rather than silently selecting an outcome"
//! when a check that could only fail through a bug fails. P9's demotion is the
//! same shape one tier up: a merge the kernel provisionally admitted, but that a
//! configured validator (build + test, in production CI) rejects, degrades to a
//! fresh Tier-3 conflict rather than standing as a silently-accepted wrong merge.
//!
//! # What this module builds
//!
//! [`dynamic_validate`] is P9 made a pure function over the kernel's
//! [`Admission`]. It takes a validation verdict (a boolean `passed`, plus a
//! human-readable `reason`) and applies it to an admission:
//!
//! * **Merged + passed** ⇒ [`Validated::Accepted`] — the provisional merge
//!   stands; validation confirmed it.
//! * **Merged + failed** ⇒ [`Validated::Demoted`] — the provisionally-admitted
//!   merge is demoted to a **real Tier-3 [`Conflict`] value** carrying the
//!   base and both sides' full content as its sides, plus the failure `reason`.
//!   The demotion never yields a silently-accepted wrong merge: a failed
//!   validation becomes a conflict *value*, consistent with I8's "no third
//!   output" and I12's "degrade to a fresh conflict".
//! * **Conflict** ⇒ [`Validated::Demoted`] carrying that conflict unchanged —
//!   the merge already conflicted, so there is nothing provisional to validate;
//!   it stays a conflict.
//!
//! The function is pure and total: no I/O, no process spawning. Running the
//! actual validator (a shell command, or in production the repository's CI) and
//! turning its exit status into the `passed` boolean is the untrusted caller's
//! job (see `omoplata-cli`'s `omo merge-file --validate`); this module only
//! encodes the *policy* — the P9 demotion — at the type level.

use crate::doc::Doc;
use crate::kernel::Admission;
use crate::merge::Conflict;

/// The outcome of running dynamic validation on a provisionally-admitted merge
/// (design doc §3 P9).
///
/// A kernel [`Admission::Merged`] is only *provisional* (§4: "Acceptance is
/// provisional pending dynamic validation (P9)"). Feeding it through
/// [`dynamic_validate`] with a validation verdict resolves it to exactly one of
/// these two terminal states — the merge stands, or it is demoted to a
/// first-class semantic conflict. There is no "accepted but unvalidated" state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Validated {
    /// Validation passed — the merge stands. Carries the merged document.
    Accepted(Doc),
    /// Validation failed (or the merge had already conflicted) — the merge is
    /// demoted to a semantic (Tier-3) conflict carrying base + both sides'
    /// intent, together with the human-readable failure `reason` (§4 Tier-3).
    Demoted {
        /// The Tier-3 conflict term: base and both sides' content as its sides.
        conflict: Conflict,
        /// Why the merge was demoted (the validator's failure explanation, or a
        /// note that the merge had already conflicted).
        reason: String,
    },
}

/// Apply a validation verdict to a kernel admission — **P9** made executable.
///
/// The verdict is supplied by the (untrusted) caller as `passed` (did the
/// configured validator — build + test, in production CI — succeed on the merged
/// tree?) plus a human-readable `reason` used when demoting. The mapping is:
///
/// * `Merged` + `passed == true`  ⇒ [`Validated::Accepted`] with the merged doc;
/// * `Merged` + `passed == false` ⇒ [`Validated::Demoted`] — a **fresh Tier-3
///   [`Conflict`]** whose `base`/`left`/`right` are `base`/`left`/`right`'s full
///   line content (both sides' intent preserved), carrying `reason`;
/// * `Conflict(..)` ⇒ [`Validated::Demoted`] carrying that conflict unchanged
///   (the first reported region, or a synthesized base/left/right term if the
///   kernel returned an empty conflict list) — nothing provisional to validate.
///
/// PROOF OBLIGATION (P9/I12 — provisional acceptance, demote-don't-accept): a
/// merge the kernel *provisionally* admitted but that fails dynamic validation
/// is **never** returned as `Accepted`. It becomes a `Conflict` value — the same
/// "degrade to a fresh conflict rather than silently selecting an outcome"
/// discipline I12 applies at resolution time, applied to P9's validation step.
/// A `passing` verdict is the *only* way to reach `Accepted`, and it is
/// reachable only from a `Merged` admission. Guarded by the module tests
/// (`merged_passed_accepts`, `merged_failed_demotes_to_tier3`,
/// `conflict_input_stays_conflicted`).
#[must_use]
pub fn dynamic_validate(
    base: &Doc,
    left: &Doc,
    right: &Doc,
    admission: Admission,
    passed: bool,
    reason: &str,
) -> Validated {
    match admission {
        Admission::Merged { result, .. } => {
            if passed {
                // The provisional merge is confirmed by dynamic validation.
                Validated::Accepted(result)
            } else {
                // A failed validation must not stand as a silently-accepted
                // wrong merge (P9). Demote to a real Tier-3 conflict value
                // carrying base + both sides' intent (I8: still a Conflict, not
                // a third output; I12: degrade rather than select an outcome).
                Validated::Demoted {
                    conflict: Conflict {
                        base: base.lines().to_vec(),
                        left: left.lines().to_vec(),
                        right: right.lines().to_vec(),
                    },
                    reason: reason.to_owned(),
                }
            }
        }
        // Already conflicted: there was nothing provisional to validate. Keep it
        // conflicted, unchanged.
        Admission::Conflict(conflicts) => {
            let conflict = conflicts.into_iter().next().unwrap_or(Conflict {
                base: base.lines().to_vec(),
                left: left.lines().to_vec(),
                right: right.lines().to_vec(),
            });
            Validated::Demoted {
                conflict,
                reason: "merge already conflicted; nothing provisional to validate".to_owned(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::admit;

    fn doc(lines: &[&str]) -> Doc {
        Doc::from_lines(lines.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn merged_passed_accepts() {
        // Disjoint edits: the kernel provisionally admits the merge.
        let base = doc(&["a", "b", "c", "d"]);
        let left = doc(&["A", "b", "c", "d"]);
        let right = doc(&["a", "b", "c", "D"]);
        let admission = admit(&base, &left, &right);
        assert!(matches!(admission, Admission::Merged { .. }));

        // Validation passes ⇒ the merge stands, carrying the merged document.
        let merged = doc(&["A", "b", "c", "D"]);
        match dynamic_validate(&base, &left, &right, admission, true, "irrelevant") {
            Validated::Accepted(result) => assert_eq!(result, merged),
            Validated::Demoted { .. } => panic!("a passing validation must accept the merge"),
        }
    }

    #[test]
    fn merged_failed_demotes_to_tier3() {
        // Same provisionally-admitted merge, but validation fails.
        let base = doc(&["a", "b", "c", "d"]);
        let left = doc(&["A", "b", "c", "d"]);
        let right = doc(&["a", "b", "c", "D"]);
        let admission = admit(&base, &left, &right);
        assert!(matches!(admission, Admission::Merged { .. }));

        match dynamic_validate(&base, &left, &right, admission, false, "build failed") {
            Validated::Demoted { conflict, reason } => {
                // The Tier-3 term carries base + both sides' intent verbatim.
                assert_eq!(conflict.base, base.lines());
                assert_eq!(conflict.left, left.lines());
                assert_eq!(conflict.right, right.lines());
                assert_eq!(reason, "build failed");
            }
            Validated::Accepted(_) => {
                panic!("a failing validation must demote, never silently accept")
            }
        }
    }

    #[test]
    fn conflict_input_stays_conflicted() {
        // Overlapping edits: the kernel already returns a conflict.
        let base = doc(&["a", "b", "c"]);
        let left = doc(&["a", "X", "c"]);
        let right = doc(&["a", "Y", "c"]);
        let admission = admit(&base, &left, &right);
        assert!(matches!(admission, Admission::Conflict(_)));

        // Even with `passed == true`, a merge that never happened cannot be
        // accepted — it stays a conflict (nothing provisional to validate).
        match dynamic_validate(&base, &left, &right, admission, true, "irrelevant") {
            Validated::Demoted { conflict, .. } => {
                assert_eq!(conflict.left, vec!["X".to_owned()]);
                assert_eq!(conflict.right, vec!["Y".to_owned()]);
            }
            Validated::Accepted(_) => panic!("a genuine conflict must never be accepted"),
        }
    }

    #[test]
    fn conflict_input_with_empty_list_synthesizes_a_term() {
        // A degenerate `Conflict(vec![])` still demotes to a well-formed Tier-3
        // term synthesized from base/left/right, never an accept.
        let base = doc(&["a"]);
        let left = doc(&["b"]);
        let right = doc(&["c"]);
        let admission = Admission::Conflict(vec![]);
        match dynamic_validate(&base, &left, &right, admission, false, "x") {
            Validated::Demoted { conflict, .. } => {
                assert_eq!(conflict.base, base.lines());
                assert_eq!(conflict.left, left.lines());
                assert_eq!(conflict.right, right.lines());
            }
            Validated::Accepted(_) => panic!("empty conflict list must still demote"),
        }
    }
}
