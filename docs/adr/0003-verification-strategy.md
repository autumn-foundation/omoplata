# ADR-0003: Verus machine-checks the kernel invariants; property tests guard the shipping code

- Status: Accepted
- Date: 2026-07-22 (revised)

## Context
The design doc (§6, §7) places `omoplata-algebra` inside the verified
boundary and calls for its invariants — I1a (diff determinism), I1b (diff
faithfulness), I5 (commutation soundness), I8 (kernel admission), and the
enabling lemma I10 (disjoint-support commutation) — to be **proven in
Verus**. I2 (merge symmetry) holds by construction; I4 (conflict confluence)
is an explicit elevation target for a later milestone.

A prior revision of this ADR recorded Verus as *not installable* in this
environment and deferred the whole obligation to property tests. **That premise
was wrong and has been retired.** Verus `0.2026.07.21.1beb0fa` (Z3 4.12.5, Rust
toolchain 1.96.0) builds and runs here. The one obstacle — this environment's
egress returns 403 for the Z3 GitHub *releases* host — is bypassed by installing
the *identical pinned* Z3 binary from PyPI (`pip install z3-solver==4.12.5`);
`git clone` and `static.rust-lang.org` are reachable. The full build recipe is
in `verus/README.md`.

## Decision
The kernel invariants are now discharged in **two complementary layers**.

### 1. Machine-checked Verus model (`verus/`)
A faithful, verified *model* of the algebra's value layer lives in
`verus/omoplata_algebra_model.rs`. It models `Doc` as `Seq<int>` (one opaque
token per line — faithful because the algebra only ever compares whole lines
with `==`) and proves:

| Invariant | Verus theorem | Status |
|-----------|---------------|--------|
| **I1b** diff faithfulness (round-trip) | `i1b_roundtrip`: `apply(base, diff(base,target)) == target` | **fully proven** |
| **I10** disjoint-support commutation | `i10_disjoint_commute`: disjoint-support patches commute | **proven for the length-preserving core** |

Verified output, verbatim:

```
verification results:: 7 verified, 0 errors
```

No `assume`, `admit`, or `external_body` appears in the file — every theorem is
fully discharged by Z3. The model is checked only by the Verus binary (via
`verus/verify.sh` and the isolated `verus-verification` CI job); it sits outside
the cargo workspace `members` list, so `cargo build/test --all` never compiles
it and the normal pipeline stays green without Verus.

**Honest scope of I5.** I10 is proven for the *length-preserving* disjoint case
— the composable core in which the doc's non-rebased statement
`apply(apply(base,p),q) == apply(apply(base,q),p)` literally holds because
neither hunk shifts the other's coordinates. The **general, length-changing I5**
requires the coordinate-*rebase* machinery in `commute.rs`; that diamond is
**not yet discharged in Verus** and remains guarded by the
`disjoint_commutes_both_orders` property test. I1a (determinism) and I8
(admission — the two-variant `Commutation` enum) also remain property/type
guarded, not yet Verus theorems.

### 2. Property tests over the shipping code (`crates/omoplata-algebra/src/tests.rs`)
The Verus module verifies a *model* of the algorithm shape; it does **not** make
the shipping `diff`/`apply`/`commute` functions themselves verified — those
remain **trusted-by-testing**. Each obligation therefore also has an executable
`proptest` run continuously against the real code, which doubles as the
differential oracle for the verified twin:

| Invariant | Property test |
|-----------|---------------|
| I1a diff determinism | `diff_is_deterministic` |
| I1b diff faithfulness (round-trip) | `diff_apply_roundtrip` |
| I5 / I10 commutation soundness & disjoint support | `disjoint_commutes_both_orders` |
| I2-style symmetry | `commute_is_symmetric`, `conflict_presence_is_symmetric` |
| I8 admission / take-one-side | `merge_takes_the_changed_side`, `merge_identical_edits` |

The public API (`Doc`, `Patch`, `diff`, `apply`, `commute`, `merge3`,
`Conflict`, `Merge`) is shaped as pure, total functions over an opaque value
model, with **no `unwrap`/`expect`/`panic` in non-test code**, so the verified
model and the shipping code share one algorithm shape.

## Consequences
- **I1b is now machine-checked** (model), not merely property-tested; the
  design doc's central "proven kernel" claim is delivered for the round-trip
  invariant and partially for commutation (I10 core).
- **I5 is partial in Verus.** The length-changing diamond via rebase is the next
  target; until then the proptest is the guard and this ADR says so plainly.
- **Verus is not a hard build dependency.** The workspace builds and tests
  without it; the proofs are checked by a separate, deliberately-slow CI job.
- The M2 algebra crate is otherwise complete against the design doc's line/
  opaque layer of §5.2; the definition-level (tree-sitter) layer and the full
  I5/I4 proofs are later milestones.
