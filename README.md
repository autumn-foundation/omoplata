# omoplata

**A version control system built on a verified merge kernel — *no silent wrong
answers*.** omoplata treats the *definition* (a function, type, or module), not
the file, as the unit of version control; records history that is bi-temporal and
queryable in both valid time (what was true) and transaction time (what was
believed); and reads and writes the git object format so it can be smuggled in as
a backend behind existing tooling. Every accepted merge is checked; everything
else degrades to an honest, first-class conflict.

> **Status: implementation in progress.** This repository scaffolds the core of
> [`Omoplata_design_doc.md`](Omoplata_design_doc.md) end to end through the `omo`
> CLI — an 8-crate workspace covering the object store, patch algebra, definition
> identity, the bi-temporal operation log, Tier-2 merge drivers, git interop, and
> the semantic layer. It is honest about its reductions: the merge kernel's
> **invariant I1b is machine-checked in Verus** (with I10 disjoint commutation
> proven for the length-preserving core; I5-proper still property-tested — see
> ADR-0003 and [`verus/`](verus/)) and the
> semantic layer uses a **deterministic hashing embedder** as its offline default,
> with **real transformer embeddings available behind an opt-in `fastembed`
> feature** (ADR-0006). The per-language structural-merge fallback is the
> real **Mergiraf** tool, integrated as a PATH-detected shell-out driver with the
> built-in line/diff3 driver as the no-tool fallback (ADR-0004). See
> [Reductions](#reductions-from-the-design-doc-in-this-build) for the full list of
> what is and is not yet implemented.

**New to omoplata?** The [User guide](docs/user-guide.md) walks a git user through
installing `omo`, an everyday quick start, a git → omo command map, migrating an
existing git repo, and the concepts (conflicts-as-values, kernel admission, the
bi-temporal op log) — with every example shown as real executed output.

## Install

Build the release binary (lands at `target/release/omo`):

```sh
cargo build --release
```

Or install the `omo` binary onto your `PATH`:

```sh
cargo install --path crates/omoplata-cli
```

## Command reference

`omo --help` lists every subcommand; `omo --version` prints the version.
Commands that operate on a repository take `--repo DIR` (defaulting to the current
directory); `init`/`status` take a positional path.

### Repository and objects

| Command | Description | Example |
|---------|-------------|---------|
| `omo init [path]` | Create a new omoplata repository (a `.omoplata/` control dir). | `omo init myrepo` |
| `omo status [path]` | Show whether a directory is an initialized repository. | `omo status myrepo` |
| `omo hash-object [--repo DIR] <path>` | Store a file as a blob and print its `sha256:` id (`-` reads stdin). | `omo hash-object README.md` |
| `omo cat-object [--repo DIR] <id>` | Print a stored object: blob bytes, or a tree listing. | `omo cat-object sha256:…` |

### Definitions

| Command | Description | Example |
|---------|-------------|---------|
| `omo defs <file.rs>` | List the Rust definitions in a file as `<kind> <path> (lines A-B)`. | `omo defs src/lib.rs` |
| `omo track <old.rs> <new.rs>` | Report definition identity across two versions: added / deleted / renamed / modified / unchanged. | `omo track old.rs new.rs` |

### Merge

| Command | Description | Example |
|---------|-------------|---------|
| `omo diff <base> <target>` | Show the line diff turning `base` into `target`, unified-ish. | `omo diff a.txt b.txt` |
| `omo merge <base> <left> <right>` | Three-way line merge; conflicts render as markers and exit non-zero. | `omo merge base left right` |
| `omo merge-file <base> <left> <right>` | Tier-2 driver merge chosen by extension: `.rs` uses the Rust structural driver; supported non-Rust files use the Mergiraf shell-out when it is on `PATH`, else the line fallback. | `omo merge-file base.json left.json right.json` |

### History and revsets

| Command | Description | Example |
|---------|-------------|---------|
| `omo ref set <name> <commit> [--repo DIR]` | Point a ref at a commit (appends a `SetRef` op to the log). | `omo ref set main sha256:…` |
| `omo ref list [--repo DIR]` | List the current refs as `name commit`. | `omo ref list` |
| `omo op log [--repo DIR]` | Print the bi-temporal operation log, newest first. | `omo op log` |
| `omo op undo [--repo DIR]` | Undo the most recent operation still in effect (total, invertible undo). | `omo op undo` |
| `omo revset <expr> [--repo DIR]` | Evaluate a revset expression (`a & b`, `a \| b`, `~a`, `all()`, `heads()`, `draft()`, `public()`, `id:<hex>`). | `omo revset 'main \| feature'` |

### Git interop

| Command | Description | Example |
|---------|-------------|---------|
| `omo git verify <git-dir>` | Run the I9 round-trip gate over every loose object; prints per-type counts and `PASS`/`FAIL`. | `omo git verify path/.git` |
| `omo git import <git-dir> [--repo DIR]` | Enforce the gate, walk the commit graph from refs, and import every reachable object (commits/tags/trees/blobs). | `omo git import path/.git` |
| `omo git log <git-dir>` | Print the imported commit graph newest-first as `<short-oid> <subject>  (parents: …)`. | `omo git log path/.git` |
| `omo git export <git-dir> <out-dir>` | Import then exact-mode export every object back out as loose objects; prints `exported N objects; round-trip vs source: PASS/FAIL`. | `omo git export path/.git out/` |

### Semantic

| Command | Description | Example |
|---------|-------------|---------|
| `omo dup <file.rs>... [--threshold T] [--real-embeddings]` | Flag likely duplicate definitions across files (convergent work before it collides). | `omo dup a.rs b.rs` |
| `omo similar <query> <file.rs>... [--top K] [--real-embeddings]` | Rank definitions by similarity to a free-text query. | `omo similar "area of rectangle" a.rs` |

`--real-embeddings` uses a real transformer model (`all-MiniLM-L6-v2`, 384-dim)
instead of the deterministic hashing stand-in. It requires the binary built with
`--features fastembed`; on first use the model (`model.onnx` ≈ 87 MB) is fetched
from HuggingFace and the ONNX Runtime from the `ort.pyke.io` CDN (both reachable
through the proxy in this environment). Without the feature, or if the hosts are
unreachable, the flag prints a note and falls back to the hashing stand-in.

## Architecture

A Cargo workspace named `omoplata`, decomposed in dependency order (§7 of the
design doc). The verified boundary is `omoplata-algebra`; everything above it is
an untrusted proposer that can produce a rejected proposal or a degraded conflict,
never a silently wrong merge.

| # | Crate | Responsibility | Design doc |
|---|-------|----------------|-----------|
| 1 | `omoplata-store` | Content-addressed object store: blobs, trees, canonical serialization, verified read-back. | §5.1, §7 #1 |
| 2 | `omoplata-algebra` | Canonical diff, patch algebra, commutation checker, conflicts-as-values — the verified core. | §5.2, §5.4, §7 #2 |
| 3 | `omoplata-identity` | Change graph, supersession, phases, and the definition graph with structural matching. | §5.3, §5.5, §7 #3 |
| 4 | `omoplata-work` | Working model: the bi-temporal operation log, total undo, and the revset engine. | §5.6, §5.8, §7 #4 |
| 5 | `omoplata-drivers` | Tier-2 structural merge (Rust via tree-sitter; Mergiraf shell-out for 45+ other languages) with a line/diff3 fallback — untrusted by design. | §4, §7 #5 |
| 6 | `omoplata-git` | Git object codec (blobs/trees/commits/tags), round-trip fidelity gate (I9), commit-graph import, and exact-mode export. | §7 #6, P8 |
| 7 | `omoplata-sem` | Embedding pipeline, semantic search, and duplicate-work detection. | §5.7, §7 #7 |
| 8 | `omoplata-cli` | The `omo` binary: command dispatch and the revset front-end. | §7 #8 |

## Design-doc traceability

Each soundness-relevant invariant from §6 is currently guarded as noted. Per
ADR-0003, I1b is **machine-checked in Verus** against a faithful model
([`verus/`](verus/)) and I10 (disjoint commutation) is Verus-checked for its
length-preserving core; the remaining invariants are guarded by executable
property or round-trip tests against the shipping code — the adversarial battery
§6 describes running against "the executable code". The Verus module verifies a
model of the algorithm shape; the shipping functions themselves stay
trusted-by-testing, with the proptests as their differential oracle.

| Invariant | Meaning | Where | Current guard |
|-----------|---------|-------|---------------|
| I1b | Diff faithfulness: `apply(a, diff(a,b)) == b` | `omoplata-algebra` | **Verus-checked (model)** + property test (round-trip) |
| I5 | Commutation soundness: commuting patches yield the same tree in either order | `omoplata-algebra` | Property test; **I10 enabling lemma Verus-checked (length-preserving core)**, general I5 in progress |
| I6 | Supersession well-formedness: the change graph is acyclic, no orphaned obsolescence | `omoplata-identity` | Unit/graph-invariant tests |
| I7 | Op-log invertibility: `undo ∘ op ≡ identity` on repository state | `omoplata-work` | Property/unit tests |
| I9 | Git round-trip fidelity: `export(import(x)) ≡ x` bit-identically | `omoplata-git` | Round-trip gate (tested, not proven — as designed) |
| I11 | Trivia conservation: merged comment tokens equal the union of both sides modulo base | `omoplata-drivers` | Structural-merge tests |
| I12/P9 | Dynamic validation: kernel admission is provisional; a failing validator demotes the merge to a Tier-3 semantic conflict rather than accepting it | `omoplata-algebra::validation`, CI `dynamic-validation` job | Unit + CLI tests; repo CI job as the concrete validator |

## Reductions from the design doc in this build

This build scaffolds the design doc's core faithfully but stands several external
systems and the formal-proof layer in with honest reductions. Read this section as
the definitive statement of what is *not* yet the real thing:

- **Verus formal proofs → checked (I1b) / partial (I5) (ADR-0003).** Verus
  `0.2026.07.21.1beb0fa` builds and runs in this environment, so the "not
  installable" premise is retired. **I1b (diff faithfulness/round-trip) is now
  machine-checked** in Verus against a faithful `Seq<int>` model
  ([`verus/`](verus/), `verified, 0 errors`), and **I10 (disjoint-support
  commutation) is proven for the length-preserving core**. The general
  length-changing **I5** (which needs coordinate rebase), plus I1a, I6, I7, I8,
  I11, I12, remain proof obligations guarded by property tests, not yet
  machine-checked. The Verus module checks a *model* of the algorithm shape; the
  shipping `diff`/`apply`/`commute` stay trusted-by-testing, with the proptests
  as their differential oracle. The design doc's "proven kernel" claim is thus
  *delivered for I1b*, *partial for I5*, and approximated elsewhere.
- **Real embedding model: opt-in, hashing stand-in by default (ADR-0006).** The
  semantic layer (`dup`, `similar`) uses a deterministic hashing embedder behind a
  pluggable `Embedder` trait as its **offline default**. A **real** transformer
  model (`all-MiniLM-L6-v2`) is now available behind the opt-in `fastembed`
  feature / `--real-embeddings` flag, since HuggingFace and the ONNX Runtime CDN
  proved reachable here; it is off by default so the default build stays offline
  and deterministic. On a semantic duplicate with different vocabulary the real
  model scores 0.72 where the stand-in scores 0.35 (and mis-ranks it below an
  unrelated pair) — the lexical-only limitation the stand-in still has by default.
- **AletheiaDB substrate → loose-object store, external-by-design (ADR-0002).**
  The object store is a git-style loose-object directory rather than an
  AletheiaDB engine — and this is not a shortfall. §3 P7 is explicit that
  *"omoplata does not build a storage engine; it defines a schema"*: AletheiaDB
  is an external substrate omoplata *targets*, not something the design doc
  specifies enough to build. So the loose store is the concrete v1 substrate,
  `Repository::{read,write}_object` is the swap-in point for a real AletheiaDB
  backend, and the bi-temporal / typed-embedding capabilities the doc ascribes
  to AletheiaDB are realized at the *schema* level here — by `omoplata-work`'s
  bi-temporal op log (§5.6) and `omoplata-sem`'s typed embeddings (§5.7) over
  the object store. The schema exists even though the named engine does not;
  building that engine is out-of-scope-by-design (ADR-0002, R5).

**Genuinely not yet implemented from the design doc:**

- The **I8 runtime kernel-admission check** (every merge result carries a checked
  commutation witness or is a Conflict value) is not hosted as a single enforced
  boundary the drivers pass through — future work.
- Git **wire protocol** (networked fetch/push) and **packfile decoding** are git
  future work. Commit-graph import and exact-mode loose-object export (closing
  the I9 `import → export → bit-identical` loop at the repository level) are
  implemented; the outstanding gap is the network protocol and packfile/delta
  decoding (loose objects round-trip end-to-end; packed objects error rather
  than being silently skipped).
- **Conflicts-as-values propagation through rebases** and **auto-rebase** (P3/P4
  beyond the op-log undo) are modeled in the algebra but not wired as a working
  rebase loop.
- **Change identity across rebase/amend** (stable Change-IDs, phases wired through
  the CLI) is present in `omoplata-identity` but not surfaced as end-to-end CLI
  workflows.
- Multi-language structural drivers **beyond Rust**, any **server/forge**, and any
  **UI beyond the CLI** are explicitly out of v1 scope.

## Development

```sh
cargo test --all                              # run the full suite
cargo fmt --all                               # format (rustfmt is canonical)
cargo clippy --all-targets -- -D warnings     # lint, warnings are errors
```

## Design and decisions

- Full design: [`Omoplata_design_doc.md`](Omoplata_design_doc.md).
- Architecture decision records: [`docs/adr/`](docs/adr/README.md) (ADR-0001 is
  the design document itself, the seed decision).
