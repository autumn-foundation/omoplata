// Machine-checked model of the `omoplata-algebra` value layer.
//
// This file is verified by the Verus binary (see `verus/verify.sh` and
// `verus/README.md`), NOT by plain `cargo`. It is a top-level `verus/`
// directory deliberately outside the cargo workspace `members` list, so a
// normal `cargo build/test --all` never sees it.
//
// WHAT THIS IS. A faithful *model* — a machine-checked twin — of the algorithm
// shape in `crates/omoplata-algebra/src/{patch.rs,commute.rs}`. It states and
// proves the design doc's kernel invariants over an abstract value model:
//
//   * I1b (diff faithfulness / round-trip): apply(base, diff(base,target)) == target
//     -> proof fn `i1b_roundtrip`  (fully discharged)
//   * I10 (disjoint-support commutation, the enabling lemma the doc says makes
//     I5 "fall out"): disjoint-support patches commute
//     -> proof fn `i10_disjoint_commute`  (discharged for the length-preserving
//        core, in which the non-rebased commutation literally holds; the
//        general length-changing case needs coordinate rebase and remains
//        property-tested — see ADR-0003).
//
// WHAT THIS IS NOT. The *shipping* `diff`/`apply`/`commute` functions are not
// themselves Verus-verified — they remain trusted-by-testing, guarded by the
// proptests in `crates/omoplata-algebra/src/tests.rs` (`diff_apply_roundtrip`,
// `disjoint_commutes_both_orders`). This module is the machine-checked
// companion of the same algorithm shape and a differential oracle for it.
//
// No `assume`, `admit`, or `external_body` is used anywhere in this file: every
// theorem below is fully discharged by Z3.

use vstd::prelude::*;

verus! {

// ---- Document abstraction ----------------------------------------------
// A `Doc` in the production crate is `Vec<String>` (an ordered list of lines).
// The algebra never inspects the *bytes* of a line; `diff`/`apply`/`commute`
// only ever compare whole lines with `==`. So we model a line by an opaque
// integer token and a document by `Seq<int>`: two lines are the "same line"
// iff their tokens are equal, which is exactly the equality the algorithm uses.
// This abstraction is faithful precisely because line-internal structure is
// invisible to the value layer.
type Doc = Seq<int>;

// ---- common prefix length ----------------------------------------------
spec fn cpl(a: Seq<int>, b: Seq<int>) -> nat
    decreases a.len(),
{
    if a.len() == 0 || b.len() == 0 {
        0
    } else if a[0] == b[0] {
        (cpl(a.subrange(1, a.len() as int), b.subrange(1, b.len() as int)) + 1) as nat
    } else {
        0
    }
}

proof fn cpl_bound(a: Seq<int>, b: Seq<int>)
    ensures
        cpl(a, b) <= a.len(),
        cpl(a, b) <= b.len(),
    decreases a.len(),
{
    if a.len() == 0 || b.len() == 0 {
    } else if a[0] == b[0] {
        cpl_bound(a.subrange(1, a.len() as int), b.subrange(1, b.len() as int));
    } else {
    }
}

// cpl(a,b) matching indices agree, index form (avoids subrange in the IH).
proof fn cpl_agree(a: Seq<int>, b: Seq<int>, k: int)
    requires
        0 <= k < cpl(a, b),
    ensures
        a[k] == b[k],
    decreases a.len(),
{
    cpl_bound(a, b);
    if a.len() == 0 || b.len() == 0 {
    } else if a[0] == b[0] {
        if k == 0 {
        } else {
            let a1 = a.subrange(1, a.len() as int);
            let b1 = b.subrange(1, b.len() as int);
            cpl_agree(a1, b1, k - 1);
            assert(a[k] == a1[k - 1]);
            assert(b[k] == b1[k - 1]);
        }
    } else {
    }
}

// ---- single-hunk faithful diff -----------------------------------------
// A hunk: at base line `start`, remove `remove`, insert `insert`. Support is
// the half-open interval [start, start+remove.len()). We build a single-hunk
// diff that keeps the common prefix as untouched context and replaces the
// remaining suffix. This is *faithful* (round-trips) though not *minimal*
// (minimality is I1a, a separate invariant explicitly out of scope here).

spec fn diff_start(base: Doc, target: Doc) -> nat {
    cpl(base, target)
}

spec fn diff_remove(base: Doc, target: Doc) -> Seq<int> {
    base.subrange(diff_start(base, target) as int, base.len() as int)
}

spec fn diff_insert(base: Doc, target: Doc) -> Seq<int> {
    target.subrange(diff_start(base, target) as int, target.len() as int)
}

// ---- faithful single-hunk apply, with context + range checks -----------
// Models production `apply` restricted to a one-hunk patch: verify the hunk
// fits (range check) and that the base slice it targets equals `remove`
// (context check); on success splice `insert` in place of `remove`. Returns
// `None` on a failed check, exactly as production `apply` returns `Err`.
spec fn apply1(base: Doc, start: nat, remove: Seq<int>, insert: Seq<int>) -> Option<Doc> {
    if start + remove.len() <= base.len() && base.subrange(
        start as int,
        (start + remove.len()) as int,
    ) == remove {
        Some(
            base.subrange(0, start as int) + insert + base.subrange(
                (start + remove.len()) as int,
                base.len() as int,
            ),
        )
    } else {
        None
    }
}

// ---- I1b: diff/apply faithfulness (round-trip) -------------------------
// design doc §6 invariant I1b: apply(base, diff(base, target)) == target.
proof fn i1b_roundtrip(base: Doc, target: Doc)
    ensures
        apply1(
            base,
            diff_start(base, target),
            diff_remove(base, target),
            diff_insert(base, target),
        ) == Some(target),
{
    let p = diff_start(base, target);
    let remove = diff_remove(base, target);
    let insert = diff_insert(base, target);
    cpl_bound(base, target);
    // range + context check pass by construction: end == base.len(),
    // remove == base[p..] definitionally.
    assert(p + remove.len() == base.len());
    assert(base.subrange(p as int, (p + remove.len()) as int) == remove);
    // prefix agreement: base[0..p] == target[0..p]
    assert(base.subrange(0, p as int) =~= target.subrange(0, p as int)) by {
        assert forall|k: int| 0 <= k < p implies base[k] == target[k] by {
            cpl_agree(base, target, k);
        }
    }
    // out == base[0..p] + insert + base[base.len()..base.len()]
    //     == target[0..p] + target[p..] == target
    let out = base.subrange(0, p as int) + insert + base.subrange(
        base.len() as int,
        base.len() as int,
    );
    assert(out =~= target) by {
        assert(base.subrange(base.len() as int, base.len() as int) =~= Seq::<int>::empty());
        assert(target.subrange(0, p as int) + target.subrange(p as int, target.len() as int)
            =~= target);
    }
}

// ---- I10 / I5: disjoint-support commutation ----------------------------
// The total splice core of `apply` under its fit precondition: overwrite the
// half-open base region [start, start+remove.len()) with `insert`.
spec fn splice(base: Doc, start: nat, remove: Seq<int>, insert: Seq<int>) -> Doc {
    base.subrange(0, start as int) + insert + base.subrange(
        (start + remove.len()) as int,
        base.len() as int,
    )
}

// A hunk is length-preserving when it inserts exactly as many lines as it
// removes; then applying it does not shift any base coordinate. This is the
// composable core in which the doc's non-rebased I10 statement
//   apply(apply(base,p),q) == apply(apply(base,q),p)   (disjoint supports)
// holds *without* the coordinate-rebase machinery. (The general
// length-changing case needs rebase; see the README/ADR note — it remains
// property-tested, not yet discharged here.)
//
// Directional core: p lies wholly before q. design doc §4 Tier 0.
proof fn i10_commute_ordered(
    base: Doc,
    ps: nat,
    prem: Seq<int>,
    pins: Seq<int>,
    qs: nat,
    qrem: Seq<int>,
    qins: Seq<int>,
)
    requires
        // p lies wholly before q (disjoint supports, p first)
        ps + prem.len() <= qs,
        qs + qrem.len() <= base.len(),
        // both hunks are length-preserving
        pins.len() == prem.len(),
        qins.len() == qrem.len(),
    ensures
        splice(splice(base, ps, prem, pins), qs, qrem, qins) == splice(
            splice(base, qs, qrem, qins),
            ps,
            prem,
            pins,
        ),
{
    let after_p = splice(base, ps, prem, pins);
    let after_q = splice(base, qs, qrem, qins);
    // Length is preserved by each splice, so all indices stay aligned.
    assert(after_p.len() == base.len());
    assert(after_q.len() == base.len());
    let lhs = splice(after_p, qs, qrem, qins);
    let rhs = splice(after_q, ps, prem, pins);
    assert(lhs =~= rhs) by {
        assert(lhs.len() == base.len());
        assert(rhs.len() == base.len());
        assert forall|i: int| 0 <= i < base.len() implies lhs[i] == rhs[i] by {
            // Five zones, each independently overwritten or untouched:
            // [0,ps) base, [ps,pe) pins, [pe,qs) base, [qs,qe) qins, [qe,len) base.
        }
    }
}

// Support half-open intervals of two hunks are disjoint: one lies wholly
// before the other. Mirrors production `hunks_conflict` returning `false`.
spec fn disjoint(ps: nat, prem_len: nat, qs: nat, qrem_len: nat) -> bool {
    ps + prem_len <= qs || qs + qrem_len <= ps
}

// design doc §6 enabling lemma I10: patches with disjoint support commute.
// Symmetric headline over the disjointness predicate (either order), for the
// length-preserving core. This is the lemma the doc says makes I5 "fall out".
proof fn i10_disjoint_commute(
    base: Doc,
    ps: nat,
    prem: Seq<int>,
    pins: Seq<int>,
    qs: nat,
    qrem: Seq<int>,
    qins: Seq<int>,
)
    requires
        disjoint(ps, prem.len(), qs, qrem.len()),
        ps + prem.len() <= base.len(),
        qs + qrem.len() <= base.len(),
        pins.len() == prem.len(),
        qins.len() == qrem.len(),
    ensures
        splice(splice(base, ps, prem, pins), qs, qrem, qins) == splice(
            splice(base, qs, qrem, qins),
            ps,
            prem,
            pins,
        ),
{
    if ps + prem.len() <= qs {
        i10_commute_ordered(base, ps, prem, pins, qs, qrem, qins);
    } else {
        // q wholly before p; apply the directional core with roles swapped.
        i10_commute_ordered(base, qs, qrem, qins, ps, prem, pins);
    }
}

fn main() {
}

} // verus!
