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
| [ADR-0008](0008-multi-writer-concurrency.md) | Multi-writer safety for `.omoplata`: advisory `flock` mutual exclusion plus crash-atomic op-log writes (temp → fsync → rename → dir fsync), with the single-writer landing daemon as the documented future. |
| [ADR-0009](0009-named-landing-queues.md) | Release lines are **named landing queues with per-queue policy** (P9 validator, approval requirement, carried-conflict rule), not branches: policy lives in the repo, validation runs before the `Draft → Public` transition, and the same change may land in several queues (backports without cherry-pick identity forks). |
| [ADR-0010](0010-distributed-omoplata.md) | **Distributed omoplata**: replicate the content-addressed store + selective ref sync, with a remote as the **landing authority** (the trust boundary is landing, not transport). Phase 1 (shipped): read-replication — `omo remote`/`omo fetch` + remote-tracking refs make `omo switch` work across repos. Phase 2 (shipped): `omo push` lands a submission on a remote through *its* policy, re-validated against *its* trunk under *its* lock. Phase 3 (shipped): `omo reconcile` folds concurrent submissions into one merged tree against their shared queue base, carrying same-definition conflicts as values instead of refusing (what git's non-fast-forward model can't); `omo land`/`omo push` do this automatically, advancing the `reconciled/<queue>` merged trunk in the landing transaction. Includes **cross-time base tracking**: each change reconciles against the trunk it was authored on (recovered from the op log via `refs_at`), so a change built on an older trunk merges correctly. Remaining future work: networked transports and attested approval. |
