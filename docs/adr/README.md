# Architecture Decision Records

This directory records the significant architecture decisions for omoplata. Each
ADR is immutable once accepted; a later decision supersedes an earlier one rather
than editing it in place.

The design document itself, [`Omoplata_design_doc.md`](../../Omoplata_design_doc.md),
is the seed decision (ADR-0001): it fixes the thesis (a verified merge kernel with
*no silent wrong answers*, definitions as the unit of version control, bi-temporal
history, non-negotiable git interop), the verified-invariant set (I1–I12), and the
eight-crate decomposition that every subsequent ADR builds on.

| ADR | Decision |
|-----|----------|
| ADR-0001 | The design document, [`Omoplata_design_doc.md`](../../Omoplata_design_doc.md) — thesis, invariants I1–I12, and the eight-crate decomposition (the seed decision). |
| [ADR-0002](0002-loose-object-store.md) | The v1 object store is a git-style loose-object directory under `.omoplata/objects/`, content-addressed with a hash-agile `ObjectId`. |
| [ADR-0003](0003-verification-strategy.md) | Verus formal proofs are deferred; the soundness-core invariants are guarded by property tests against the executable code, with proof obligations documented. |
| [ADR-0004](0004-merge-drivers.md) | Tier-2 merge uses a Rust structural driver with a built-in line/diff3 fallback, standing in for the Mergiraf adapter named in the design doc. |
| [ADR-0005](0005-git-interop-roundtrip-gate.md) | Git interop is a byte-faithful object codec gated by an `export(import(x)) == x` round-trip check (invariant I9). |
| [ADR-0006](0006-semantic-embeddings.md) | The embedding model is a deterministic local stand-in behind a pluggable `Embedder` trait, so a real model can be swapped in without touching callers. |
| [ADR-0007](0007-dynamic-validation.md) | Kernel admission is provisional (P9): a configured dynamic validator (in production, CI) runs against the merged tree, and a failure demotes the merge to a Tier-3 semantic conflict rather than accepting a merge that doesn't build/test. Realizes the per-instance I12 guard; the repo's own CI job is the concrete validator. |
