# ADR-0005: git interop is a byte-faithful codec behind a round-trip gate (I9)

- Status: Accepted
- Date: 2026-07-22
- Updated: 2026-07-22 (M10 — commit-graph import and exact-mode export)

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

### Commit graph and export (M10)
The codec parses commits into `GitCommit { tree, parents, author, committer,
message }` and annotated tags into `GitTag { object, kind, tag, tagger,
message }`. Both **also retain their raw body bytes**, so re-encoding is
byte-identical regardless of headers the typed view does not model — `gpgsig`
(and its space-prefixed continuation lines), `mergetag`, `encoding`, extra or
reordered headers, and the exact timestamp/timezone formatting of the identity
lines. This is the design doc's sanctioned trade (§3 P8's byte-identity gate
over faithful modelling): the typed fields are real and drive the DAG, while
byte-identity leans on the untouched bytes. `encode(decode(bytes)) == bytes` is
proved on real signed commits and annotated tags built via `git` in guarded
tests.

`read_refs` reads `HEAD`, loose refs under `refs/`, and `packed-refs`
(skipping `#` comment and `^peeled` lines), resolving symbolic refs to the oid
they name and returning a name-sorted list.

`import_repo` runs the gate first and **refuses to import** if it fails
(I9 enforcement), then **walks the commit graph from every ref** — following
commit parents, commit trees and their subtrees, and annotated-tag targets —
importing every reachable blob → `Object::Blob` and tree → `Object::Tree`,
recording the commit DAG (`commit_oid → GitCommit`), the ref list, and the
git→omoplata blob/tree map. Every reachable object is re-checked through
`roundtrip_ok` as it is visited.

`export_repo` writes every imported object back out as a loose object,
reconstructed from the decoded model (so byte-identical to the source), plus
its refs, under an `objects/<xx>/<38>` + `refs/…` layout.
`export_matches_source` is the repo-level round-trip gate: it confirms the
exported loose-object set has exactly the same object oids and the same object
bytes as the source. **"Byte-identical" is defined at the git-object level** —
the SHA-1-committed uncompressed canonical form `"{type} {len}\0{body}"`, *not*
the zlib-compressed loose-file bytes. zlib compression is not uniquely
determined (compressor, level, and version all vary the bytes) and git itself
does not promise identical compressed bytes across versions; two loose files
with the same oid decompress to the same object and are, by construction, the
same object. Discharging I9 at the object level is discharging it at the level
the oid — and git's own integrity model — defines.

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
- **Packfile scope.** v1 decodes **loose objects only**. If the commit-graph
  walk reaches an object that is not a loose object and the repo has packfiles,
  import fails with `PackedObject` (a count is surfaced) rather than silently
  skipping it; `verify_repo` reports a packfile count so the CLI never claims a
  false whole-repo PASS over a repo whose objects are packed. Fresh
  `git init` + commits produce loose objects, which the pipeline handles
  end-to-end. Packfile (and delta) decoding is future work.
- **Future work.** The git **wire protocol** (networked fetch/push) is out of
  the offline-feasible scope and remains future work, as does **packfile
  decoding**. The commit-graph import and exact-mode loose-object export
  (the outbound half of I9's equation) are now implemented (M10).
- **Exact-export path.** Export reconstructs objects from the decoded git-side
  model (`GitImport::git_objects`), *not* from the omoplata trees — the git↔
  omoplata tree mapping is lossy (see the fidelity caveat above), so the
  authoritative git bytes are what round-trip. Byte-identity is asserted at the
  object level (uncompressed canonical form), the level the oid commits to.
- Non-UTF-8 tree entry names are not supported in v1 (names are modelled as
  `String`); likewise a commit/tag whose header block is not valid UTF-8 fails
  `decode`. Such an object errors rather than importing lossily. (The commit/
  tag *message* is non-UTF-8-tolerant: it is retained in the raw body for exact
  re-encoding and only the display copy is decoded lossily.)
