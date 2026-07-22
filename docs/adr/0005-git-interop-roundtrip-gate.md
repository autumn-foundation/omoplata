# ADR-0005: git interop is a byte-faithful codec behind a round-trip gate (I9)

- Status: Accepted
- Date: 2026-07-22

## Context
The design doc makes git interoperability non-negotiable (§3 P8): omoplata
must read and write the git object format, and *"round-trip fidelity (`git
repo → import → export → bit-identical`) is a release gate."* Invariant I9
(§6) states this precisely — `export(import(git_repo)) ≡ git_repo`
bit-identically — and explicitly declines to prove it in Verus: *"held as a
fuzz-tested release gate rather than a Verus theorem (the git format's warts
resist clean modeling)."* Scope (§8) bounds the effort: *"git interop with
round-trip gate"* is in; *"SHA-1 interop beyond what git import requires"* is
out.

## Decision
`omoplata-git` (crate #6, "Unverified, mandatory") implements a faithful git
object codec — `encode`/`decode`/`oid` over `GitObject {Blob, Tree, Commit,
Tag}` — where trees re-encode **byte-identically** (mode strings kept
verbatim, entry order preserved, raw 20-byte oids untouched). Objects are
addressed by the SHA-1 of the uncompressed `"{type} {len}\0{body}"` form and
stored zlib-compressed as loose files at `objects/<xx>/<38 hex>`.

I9 is discharged empirically, not proven:
- `roundtrip_ok(bytes)` decodes, re-encodes, asserts byte-identity, and only
  then returns the oid — the executable round-trip guarantee.
- `verify_repo(git_dir)` runs that gate over every loose object and confirms
  each recomputed SHA-1 equals its on-disk path.
- Property tests exercise the gate over arbitrary blob bytes and arbitrary
  tree entries; a guarded integration test runs it against a real `git`
  repository.

`import_repo` runs the gate first and **refuses to import** if it fails
(I9 enforcement), then maps git blobs → `Object::Blob` and git trees →
`Object::Tree`.

Dependencies are pure-Rust: `flate2` (default miniz_oxide backend — the
zlib-ng/system feature is deliberately **not** enabled) and RustCrypto
`sha1`. No system libraries.

## Consequences
- The gate is a hard precondition on import: a repo that does not round-trip
  cannot be imported, matching the "release gate" posture.
- **Fidelity caveat (git ↔ omoplata tree mapping).** The omoplata tree model
  distinguishes only `Blob` vs `Tree`, whereas git carries a full octal mode
  per entry. Modes `100644`, `100755`, and `120000` all collapse to
  `EntryKind::Blob`, so the executable bit and symlink-ness are not
  recoverable from the omoplata tree alone. Exact git export must consult the
  git-side record, which `GitImport` keeps authoritative in `git_objects`
  (decoded objects keyed by original oid). The round-trip gate itself operates
  entirely on the git side and is therefore unaffected by this lossiness.
- **Future work.** The commit graph is not modelled in v1: commits and tags
  are counted and their reachable blobs/trees imported, but parent/tree edges
  are not materialized in the omoplata store. The git wire protocol and a
  full `export` path (the other half of I9's equation) are also future work;
  v1 lands the codec, the gate, and inbound import.
- Non-UTF-8 tree entry names are not supported in v1 (names are modelled as
  `String`); such an object fails `decode` rather than importing lossily.
