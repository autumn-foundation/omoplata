# ADR-0003: Verus formal proofs deferred; invariants guarded by property tests

- Status: Accepted
- Date: 2026-07-22

## Context
The design doc (§6, §7) places `omoplata-algebra` inside the verified
boundary and calls for its invariants — I1a (diff determinism), I1b (diff
faithfulness), I5 (commutation soundness), I8 (kernel admission), and the
enabling lemma I10 (disjoint-support commutation) — to be **proven in
Verus**. I2 (merge symmetry) holds by construction; I4 (conflict confluence)
is an explicit elevation target for a later milestone.

Verus is **not installed** in this environment (`which verus` and
`which cargo-verus` both fail), and installing the Verus toolchain here is out
of scope for M2. The standing instruction permits a documented stub in place
of the formal proof, provided the gap is flagged and the design is drawn so the
proof can be added later.

## Decision
For M2, every proof obligation on the algebra is encoded **twice**:

1. As a `// PROOF OBLIGATION (Ix): <property>` comment sited at the function
   that must satisfy it (`diff`, `apply`, `commute`, `merge3`), naming the
   design-doc invariant.
2. As an executable `proptest` property in
   `crates/omoplata-algebra/src/tests.rs`, so the invariant is checked
   continuously against the real code:

   | Invariant | Property test |
   |-----------|---------------|
   | I1a diff determinism | `diff_is_deterministic` |
   | I1b diff faithfulness (round-trip) | `diff_apply_roundtrip` |
   | I5 / I10 commutation soundness & disjoint support | `disjoint_commutes_both_orders` |
   | I2-style symmetry | `commute_is_symmetric`, `conflict_presence_is_symmetric` |
   | I8 admission / take-one-side | `merge_takes_the_changed_side`, `merge_identical_edits` |

The public API (`Doc`, `Patch`, `diff`, `apply`, `commute`, `merge3`,
`Conflict`, `Merge`) is deliberately shaped as pure, total functions over an
opaque value model, with **no `unwrap`/`expect`/`panic` in non-test code**, so a
Verus specification can be attached to these exact signatures later **without an
API change**.

## Consequences
- **This is a documented stub, flagged as such.** The invariants are currently
  guarded by property-based tests, not machine-checked proofs. A regression
  that a proof would have caught is only caught to the coverage of the
  generators (64 cases per property over a small line alphabet).
- Adding Verus later is additive: drop in the `verus!` specs against the
  existing functions; the property tests remain as the design doc's
  complementary adversarial battery (§6, Havoc).
- The M2 algebra crate is otherwise complete against the design doc's line/
  opaque layer of §5.2; the definition-level (tree-sitter) layer and the actual
  Verus proofs are later milestones.
