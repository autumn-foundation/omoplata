# ADR-0002: v1 object store is a git-style loose-object directory

- Status: Accepted
- Date: 2026-07-22

## Context
The design doc (§3 P7, §5.1) specifies the substrate as AletheiaDB — a
content-addressed, bi-temporal object store. It commits to *properties*
(SHA-256 addressing, a hash-agile envelope, round-trip-verified
serialization) but not to a concrete on-disk backend, and does not say
whether `omoplata-store` should embed AletheiaDB in v1. (ADR-0001 is the
design document itself.)

## Decision
For v1, `omoplata-store` persists objects as **loose files** under
`.omoplata/objects/<alg>/<xx>/<rest>`, addressed by a hash-agile `ObjectId`
(`<alg>:<hex>`, SHA-256 today). Each object carries a self-describing
envelope `"{kind} {len}\0{payload}"`. This keeps the crate dependency-free
beyond `sha2`, trivially testable, and faithful to the doc's required
properties, while keeping the backend behind `Repository::{read,write}_object`
so it can be swapped for AletheiaDB without touching callers.

## Consequences
- No external storage engine is needed to build or test the merge kernel
  that depends on this crate (§9 M1 pairs store + algebra).
- The on-disk format is an internal detail; the public surface is the object
  model plus `read_object`/`write_object`.
- The bi-temporal graphs (change/definition/op log) are out of scope for
  this crate — they belong to `omoplata-identity`.

## R5 — AletheiaDB is external-by-design, not a gap to close (2026-07-22)
The reductions burn-down re-examined whether "AletheiaDB substrate → loose
store" is a shortfall to be closed by building the named engine. It is not, and
the design doc says so explicitly. §3 P7 states, verbatim:

> **P7 — The substrate is AletheiaDB.** The commit graph, change-identity
> graph, definition graph, and operation log are interleaved temporal graphs
> over one content-addressed object store — which is AletheiaDB's native data
> model (bi-temporal, multiple typed embeddings per node, independently
> indexed). **omoplata does not build a storage engine; it defines a schema.**
> Embeddings on every node come effectively free and power the semantic layer
> (§7).

The doc's own scoping settles it: omoplata *targets* AletheiaDB (a bi-temporal,
content-addressed, multiple-typed-embeddings-per-node substrate — see also the
§4 architecture "AletheiaDB substrate" subgraph, §5.1 "stored in AletheiaDB",
and §5.7 "typed embeddings (AletheiaDB native)") but never specifies the engine
in enough detail to build one, because building it was never omoplata's job.
This reclassifies the ADR-0002 reduction from an *assumed* substitution to one
**evidenced by the doc's own scoping**: shipping a schema over a concrete object
store, rather than an AletheiaDB engine, is exactly what the design mandates.
Writing a storage engine here would be out-of-scope-by-design.

The v1 loose-object store above is that concrete substrate, and
`Repository::{read,write}_object` is the swap-in point for a real AletheiaDB
backend when one exists — no caller changes.

Crucially, the *capabilities* the doc attributes to AletheiaDB are not lost by
not having the engine — they are realized **at the schema level** over the
object store, which is precisely what P7 says omoplata's job is:

- **Bi-temporal history** (valid time + transaction time) is realized by
  `omoplata-work`'s bi-temporal operation log (§5.6, invariant I7), which
  records every mutation with transaction time and total invertible undo over
  the object store — the schema exists even though the named engine does not.
- **Multiple typed embeddings per node** are realized by `omoplata-sem`'s
  embedding pipeline (§5.7): typed embeddings computed over object-store
  content and indexed for duplicate-work detection and semantic search,
  pluggable via the `Embedder` trait (ADR-0006).

So AletheiaDB the engine is external and future; AletheiaDB the *schema* —
interleaved temporal graphs with per-node embeddings over a content-addressed
store — is present, built over the loose store rather than under it.
