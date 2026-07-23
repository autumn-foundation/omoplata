# ADR-0005: git interop is a byte-faithful codec behind a round-trip gate (I9)

- Status: Accepted
- Date: 2026-07-22
- Updated: 2026-07-22 (M10 — commit-graph import and exact-mode export)
- Updated: 2026-07-22 (R3 — git wire protocol fetch over the local transport)

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

### Wire protocol — `upload-pack` fetch over the local transport (R3)
§3 P8 requires omoplata to read and write *"the git object format **and wire
protocol**."* Git speaks the same pkt-line + `upload-pack`/`receive-pack`
conversation over every transport; only the process/socket plumbing differs.
For `file://` URLs and local paths git uses the **local transport**, which
spawns the server-side helper (`git upload-pack` for fetch, `git receive-pack`
for push) and speaks the protocol over that child process's stdio. R3
implements the fetch half of this as a genuine wire-protocol client, run against
a local `upload-pack` process rather than a socket.

The `wire` module provides:
- **`pkt`** — the transport-agnostic pkt-line codec: `write_pkt_line`/
  `read_pkt_line`, the flush/delim/response-end markers, and ref-line parsing.
  Unit-tested on known vectors (`write_pkt_line(b"hello\n") == b"000ahello\n"`,
  flush `== b"0000"`, empty payload `== b"0004"`).
- **`fetch_local(url_or_path, repo)`** — spawns `git upload-pack <dir>` (protocol
  v0; `GIT_PROTOCOL` is scrubbed so an inherited v2 setting cannot change the
  framing), reads the **ref advertisement**, sends `want <oid> ofs-delta` lines
  for the advertised refs plus a flush and a `done`, reads the `NAK`
  acknowledgement, and collects the **raw packfile bytes** to EOF. It
  deliberately requests `ofs-delta` but **not** `side-band`/`side-band-64k`, so
  the pack arrives as a raw stream rather than multiplexed pkt-lines.

The received pack has no sidecar `*.idx`, so `pack::parse_pack_bytes` decodes it
**in memory**: it walks the pack sequentially (discovering each object's offset
via the zlib stream's compressed length), resolves `OFS_DELTA` bases by
back-offset and `REF_DELTA` bases by oid (deferral passes handle out-of-order
ref-delta bases), and recomputes each object's SHA-1 from its reconstructed
content. A full clone's pack is self-contained (not thin), so every base
resolves within it. The decoded objects are imported through the **same I9
gate** as on-disk import: `import_objects` — the shared core behind both
`import_repo` and the wire path — runs `roundtrip_ok` on every object and reuses
the M6/M10 blob/tree import mapping. This reuses the existing packfile
delta/`apply_delta` machinery; the parser is not duplicated.

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
- **Packfile scope.** Packfile and delta decoding (`parse_idx`, `read_pack`, `parse_pack_bytes`) are fully implemented, resolving `OFS_DELTA` and `REF_DELTA` delta chains. `import_repo`, `verify_repo`, and `export_repo` decode loose objects and packed objects alike into an integrated `GitImport`, ensuring `git gc`'d repositories pass the I9 round-trip gate identically to loose repositories. `packfiles` counts in `GitReport` are retained as informational.
- **Wire-protocol scope and future work.** R3 implements the **fetch**
  (`upload-pack`) half of the wire protocol over the **local transport**
  (`file://` / local paths). Still future work:
  - **Push (`receive-pack`) over the local transport.** Not implemented in R3.
    The pkt-line codec is transport- and direction-agnostic and would be reused;
    a push additionally needs ref-update commands (`<old> <new> <ref>`) and our
    own packfile *writer* (serialize + delta-compress the objects to send),
    which the current crate does not have (it decodes packs, it does not encode
    them). Deferred to keep R3 scoped to the read path.
  - **Networked transports (`http`/`ssh`).** The protocol code here is exactly
    what those transports drive; only the process/socket plumbing differs. They
    are not offline-testable in this environment, so they remain out of scope.
  The commit-graph import and exact-mode loose-object export (the outbound half
  of I9's equation) were implemented in M10; packfile (and delta) decoding is
  implemented and now also feeds the in-memory wire-pack decoder.
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
