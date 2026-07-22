//! Unit tests for concrete cases and a `proptest` battery encoding the design
//! doc's algebra invariants (I1a, I1b, I5, I8, I10, and the by-construction
//! symmetry of I2) as executable properties.
//!
//! These property tests guard the *shipping* `diff`/`apply`/`commute` code (they
//! remain trusted-by-testing) and act as the differential oracle for the
//! machine-checked companion in the top-level `verus/` directory. That Verus
//! model proves **I1b** (diff round-trip) outright and **I10** (disjoint-support
//! commutation) for the length-preserving core; the general length-changing
//! **I5** is still only property-tested here (`disjoint_commutes_both_orders`).
//! See `verus/README.md` and `docs/adr/0003-verification-strategy.md`.

use super::*;
use crate::commute::combine;

fn doc(lines: &[&str]) -> Doc {
    Doc::from_lines(lines.iter().map(|s| (*s).to_owned()).collect())
}

// ----------------------------------------------------------------------------
// Concrete unit tests
// ----------------------------------------------------------------------------

#[test]
fn doc_roundtrips_through_string() {
    for s in ["", "a", "a\n", "a\nb", "a\nb\n", "\n\n", "x\ny\nz\n"] {
        assert_eq!(
            Doc::from_str(s).to_string(),
            s,
            "round-trip failed for {s:?}"
        );
    }
}

#[test]
fn diff_of_simple_docs() {
    let base = doc(&["a", "b", "c"]);
    let target = doc(&["a", "x", "c"]);
    let patch = diff(&base, &target);
    assert_eq!(patch.hunks().len(), 1);
    let h = &patch.hunks()[0];
    assert_eq!(h.base_start, 1);
    assert_eq!(h.remove, vec!["b".to_owned()]);
    assert_eq!(h.insert, vec!["x".to_owned()]);
    // Support is the single touched base line [1, 2).
    assert_eq!(patch.support(), vec![1..2]);
    // I1b — faithfulness.
    assert_eq!(apply(&patch, &base).unwrap(), target);
}

#[test]
fn diff_pure_insertion_has_empty_support() {
    let base = doc(&["a", "c"]);
    let target = doc(&["a", "b", "c"]);
    let patch = diff(&base, &target);
    assert_eq!(patch.hunks().len(), 1);
    let h = &patch.hunks()[0];
    assert!(h.remove.is_empty());
    assert_eq!(h.insert, vec!["b".to_owned()]);
    // An insertion touches no base line: empty interval anchored at base_start.
    assert_eq!(patch.support(), vec![1..1]);
    assert_eq!(apply(&patch, &base).unwrap(), target);
}

#[test]
fn apply_context_mismatch_errors() {
    let base = doc(&["a", "b", "c"]);
    let bad = Patch::from_hunks(vec![Hunk {
        base_start: 1,
        remove: vec!["WRONG".to_owned()],
        insert: vec!["x".to_owned()],
    }]);
    assert_eq!(
        apply(&bad, &base),
        Err(ApplyError::ContextMismatch { base_start: 1 })
    );
}

#[test]
fn apply_out_of_range_errors() {
    let base = doc(&["a", "b"]);
    let bad = Patch::from_hunks(vec![Hunk {
        base_start: 2,
        remove: vec!["c".to_owned()],
        insert: vec![],
    }]);
    assert_eq!(
        apply(&bad, &base),
        Err(ApplyError::OutOfRange {
            base_start: 2,
            remove_len: 1,
            base_len: 2,
        })
    );
}

#[test]
fn disjoint_patches_commute() {
    let base = doc(&["a", "b", "c", "d"]);
    let left = doc(&["A", "b", "c", "d"]); // edits line 0
    let right = doc(&["a", "b", "c", "D"]); // edits line 3
    let p = diff(&base, &left);
    let q = diff(&base, &right);
    match commute(&p, &q) {
        Commutation::Independent {
            p_rebased,
            q_rebased,
        } => {
            let one = combine(&base, &p, &q_rebased).unwrap();
            let two = combine(&base, &q, &p_rebased).unwrap();
            assert_eq!(one, two);
            assert_eq!(one, doc(&["A", "b", "c", "D"]));
        }
        Commutation::Overlap => panic!("disjoint edits should commute"),
    }
}

#[test]
fn overlapping_patches_do_not_commute() {
    let base = doc(&["a", "b", "c"]);
    let left = doc(&["a", "X", "c"]);
    let right = doc(&["a", "Y", "c"]);
    let p = diff(&base, &left);
    let q = diff(&base, &right);
    assert_eq!(commute(&p, &q), Commutation::Overlap);
}

#[test]
fn merge_clean_one_sided_left() {
    let base = doc(&["a", "b", "c"]);
    let left = doc(&["a", "X", "c"]);
    let m = merge3(&base, &left, &base);
    assert!(m.is_clean());
    assert_eq!(m.merged, left);
}

#[test]
fn merge_clean_one_sided_right() {
    let base = doc(&["a", "b", "c"]);
    let right = doc(&["a", "Y", "c"]);
    let m = merge3(&base, &base, &right);
    assert!(m.is_clean());
    assert_eq!(m.merged, right);
}

#[test]
fn merge_clean_disjoint_both_sides() {
    let base = doc(&["a", "b", "c", "d"]);
    let left = doc(&["A", "b", "c", "d"]);
    let right = doc(&["a", "b", "c", "D"]);
    let m = merge3(&base, &left, &right);
    assert!(m.is_clean());
    assert_eq!(m.merged, doc(&["A", "b", "c", "D"]));
}

#[test]
fn merge_one_concrete_conflict() {
    let base = doc(&["a", "b", "c"]);
    let left = doc(&["a", "X", "c"]);
    let right = doc(&["a", "Y", "c"]);
    let m = merge3(&base, &left, &right);
    assert!(!m.is_clean());
    assert_eq!(m.conflicts.len(), 1);
    assert_eq!(
        m.conflicts[0],
        Conflict {
            base: vec!["b".to_owned()],
            left: vec!["X".to_owned()],
            right: vec!["Y".to_owned()],
        }
    );
    // The reconstructed doc renders the conflict with deterministic markers.
    assert_eq!(
        m.merged,
        doc(&[
            "a",
            CONFLICT_START,
            "X",
            CONFLICT_SEP,
            "Y",
            CONFLICT_END,
            "c",
        ])
    );
}

// ----------------------------------------------------------------------------
// Property tests (the executable stand-ins for the Verus proofs)
// ----------------------------------------------------------------------------

use proptest::prelude::*;

/// Lines drawn from a small alphabet so random docs collide, overlap, and
/// conflict often enough to exercise every path.
fn line_strategy() -> impl Strategy<Value = String> {
    prop::sample::select(vec!["a", "b", "c", "d", "x", "y"]).prop_map(str::to_owned)
}

fn doc_strategy() -> impl Strategy<Value = Doc> {
    prop::collection::vec(line_strategy(), 0..8).prop_map(Doc::from_lines)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    // I1a — diff determinism: identical inputs yield bit-identical patches.
    #[test]
    fn diff_is_deterministic(a in doc_strategy(), b in doc_strategy()) {
        prop_assert_eq!(diff(&a, &b), diff(&a, &b));
    }

    // I1b — diff faithfulness (the round-trip theorem):
    // apply(diff(a, b), a) == Ok(b).
    #[test]
    fn diff_apply_roundtrip(a in doc_strategy(), b in doc_strategy()) {
        prop_assert_eq!(apply(&diff(&a, &b), &a), Ok(b));
    }

    // Doc string round-trip is the identity on bytes.
    #[test]
    fn doc_string_roundtrip(s in "[a-d\n]{0,12}") {
        prop_assert_eq!(Doc::from_str(&s).to_string(), s);
    }

    // I5 / I10 — commutation soundness and disjoint-support commutation:
    // whenever `commute` admits a pair, applying in either order yields the
    // identical document.
    #[test]
    fn disjoint_commutes_both_orders(
        base in doc_strategy(),
        left in doc_strategy(),
        right in doc_strategy(),
    ) {
        let p = diff(&base, &left);
        let q = diff(&base, &right);
        if let Commutation::Independent { p_rebased, q_rebased } = commute(&p, &q) {
            let one = combine(&base, &p, &q_rebased);
            let two = combine(&base, &q, &p_rebased);
            prop_assert!(one.is_ok(), "p then q_rebased failed to apply");
            prop_assert!(two.is_ok(), "q then p_rebased failed to apply");
            prop_assert_eq!(one, two);
        }
    }

    // `commute` is symmetric: Independent iff swapped is Independent.
    #[test]
    fn commute_is_symmetric(
        base in doc_strategy(),
        left in doc_strategy(),
        right in doc_strategy(),
    ) {
        let p = diff(&base, &left);
        let q = diff(&base, &right);
        let forward = matches!(commute(&p, &q), Commutation::Independent { .. });
        let backward = matches!(commute(&q, &p), Commutation::Independent { .. });
        prop_assert_eq!(forward, backward);
    }

    // I8 (take-one-side): a merge where one side is unchanged is clean and
    // equals the changed side.
    #[test]
    fn merge_takes_the_changed_side(base in doc_strategy(), a in doc_strategy()) {
        let left_only = merge3(&base, &a, &base);
        prop_assert!(left_only.is_clean());
        prop_assert_eq!(&left_only.merged, &a);

        let right_only = merge3(&base, &base, &a);
        prop_assert!(right_only.is_clean());
        prop_assert_eq!(&right_only.merged, &a);
    }

    // Identical edits on both sides merge cleanly to that edit.
    #[test]
    fn merge_identical_edits(base in doc_strategy(), a in doc_strategy()) {
        let m = merge3(&base, &a, &a);
        prop_assert!(m.is_clean());
        prop_assert_eq!(m.merged, a);
    }

    // Conflict presence is symmetric under swapping left and right (cf. I2).
    #[test]
    fn conflict_presence_is_symmetric(
        base in doc_strategy(),
        left in doc_strategy(),
        right in doc_strategy(),
    ) {
        let fwd = merge3(&base, &left, &right);
        let bwd = merge3(&base, &right, &left);
        prop_assert_eq!(fwd.is_clean(), bwd.is_clean());
        prop_assert_eq!(fwd.conflicts.len(), bwd.conflicts.len());
    }
}
