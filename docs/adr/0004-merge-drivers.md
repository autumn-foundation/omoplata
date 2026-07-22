# ADR-0004: Tier-2 merge drivers — Rust structural + built-in line fallback (Mergiraf stand-in)

- Status: Accepted
- Date: 2026-07-22

## Context
The design doc's merge pipeline (§4) escalates surviving conflicts to a
per-language **Tier-2 structural** driver, and names the interim/fallback driver
explicitly. From §8 scope:

> Tier-2 structural merge for **Rust only** (one grammar, dogfooded on the
> Autumn stack), **Mergiraf as the fallback driver for everything else**.

and the §4 architecture diagram lists an "Interim driver: Mergiraf" among the
untrusted proposers. The crate table (§7) marks `omoplata-drivers` as
**Untrusted by design**: drivers are *proposers* under the LCF architecture
(principle **P1**). Their output is a candidate merge that the verified kernel
admits only after checking tree equality and **trivia conservation (I11)**;
a bad or failed driver can only produce a rejected proposal or an honest
conflict, never a silently wrong merge.

Two facts shape this milestone (M5):

1. **Mergiraf is an external binary and is not vendored into this environment.**
   Wiring a hard dependency on a tool that is not present would make the crate
   unbuildable/untestable here.
2. **tree-sitter is error-tolerant.** It recovers from malformed input and
   still returns a best-effort tree with `ERROR` / `MISSING` nodes, so a naive
   structural driver would happily merge partially-parsed trees.

## Decision
`omoplata-drivers` ships two drivers behind a `MergeDriver` trait, selected by
file extension (`select_driver`):

- **`RustStructuralDriver` (`"rust-structural"`)** — the Tier-2 structural merge
  for Rust, the point of M5. It merges at **definition granularity** using the
  tree-sitter extraction and tiered identity matcher from `omoplata-identity`
  (P6): base/left/right are split into top-level items (+ the inter-item text,
  so reassembly is byte-faithful), items are paired across versions by identity,
  and the merged item set is assembled in a documented canonical order
  (surviving base items in base order, then left-added, then right-added). A
  definition edited on both sides is line-merged internally via
  `omoplata_algebra::merge3`; unresolvable cases degrade to a first-class
  `Conflict` value. This succeeds where a pure line merge conflicts — e.g. two
  branches each appending a new item at the same textual location.

- **`LineDriver` (`"line"`)** — a diff3-style line merge wrapping the verified
  `omoplata_algebra::merge3`, used for every non-`.rs` path.

### The Mergiraf substitution (flagged)
**The built-in `LineDriver` stands in for the design doc's named Mergiraf
fallback.** Mergiraf is an external binary not bundled in this environment, so
the honest built-in fallback (diff3 over lines) is the *current* fallback
driver. In the full system the fallback slot would delegate to `mergiraf` when
it is available on `PATH`; that shelling-out is **not** implemented here, and
the built-in fallback is designed to work with no external tool. This is a
deliberate substitution and is called out as such rather than pretending
Mergiraf is wired.

### Parse fallback for the structural driver
Because tree-sitter recovers from malformed input, the structural driver checks
that all three sides parse cleanly (no error nodes) via the new
`omoplata_identity::parses_cleanly` helper. If any side is malformed — or a hard
grammar/parse error occurs — it **falls back to `LineDriver`** and returns that
output (whose `driver` field is honestly `"line"`), rather than structurally
merging a broken tree. This keeps the driver safe on partial/invalid sources.

## Consequences
- **Untrusted, unverified, no kernel check yet.** These drivers sit outside the
  verified boundary by design. This crate does not yet host the kernel admission
  check (tree equality + I11 trivia conservation) that would gate a structural
  proposal in the full system; that wiring is a later milestone. The driver's
  own discipline is the I8-style honest-degradation rule: every result is a
  clean merge or a `Conflict` value, never a silent drop or silent side-pick.
- **A new public helper in `omoplata-identity`.** `parses_cleanly` was added to
  expose tree-sitter's error state (which `extract_definitions` swallows), so
  the driver can detect malformed input and degrade. It is additive and does not
  change existing behavior.
- **Trivia placement is approximate at v1.** Inter-item text (blank lines, free
  comments, doc comments not owned by an item's node) is preserved positionally
  during reassembly, but the design doc's full Roslyn-style trivia-ownership
  policy and the kernel's I11 conservation check are not yet implemented here.
  This matches the doc's staging (§4 Tier 2, Q2) and is a documented gap.
- **Optional Mergiraf detection is deferred.** Detecting `mergiraf` on `PATH`
  and shelling out is a clean future addition behind the same `MergeDriver`
  trait; the built-in fallback must (and does) work without it.
