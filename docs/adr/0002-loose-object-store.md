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
