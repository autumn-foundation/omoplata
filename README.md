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
> **formal Verus proofs are deferred to property tests** (ADR-0003), the
> per-language structural-merge fallback is a **built-in line/diff3 driver**
> standing in for Mergiraf (ADR-0004), and the semantic layer uses a
> **deterministic hashing embedder** standing in for a real embedding model
> (ADR-0006). See [Reductions](#reductions-from-the-design-doc-in-this-build) for
> the full list of what is and is not yet implemented.

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
| `omo merge-file <base> <left> <right>` | Tier-2 driver merge chosen by extension: `.rs` uses the Rust structural driver, everything else the line fallback. | `omo merge-file base.rs left.rs right.rs` |

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
| `omo git import <git-dir> [--repo DIR]` | Enforce the gate, then import git blobs and trees into the store. | `omo git import path/.git` |

### Semantic

| Command | Description | Example |
|---------|-------------|---------|
| `omo dup <file.rs>... [--threshold T]` | Flag likely duplicate definitions across files (convergent work before it collides). | `omo dup a.rs b.rs` |
| `omo similar <query> <file.rs>... [--top K]` | Rank definitions by similarity to a free-text query. | `omo similar "area of rectangle" a.rs` |

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
| 5 | `omoplata-drivers` | Tier-2 structural merge (Rust via tree-sitter) with a line/diff3 fallback — untrusted by design. | §4, §7 #5 |
| 6 | `omoplata-git` | Git object codec and the round-trip fidelity gate (I9); import into the store. | §7 #6, P8 |
| 7 | `omoplata-sem` | Embedding pipeline, semantic search, and duplicate-work detection. | §5.7, §7 #7 |
| 8 | `omoplata-cli` | The `omo` binary: command dispatch and the revset front-end. | §7 #8 |

## Design-doc traceability

Each soundness-relevant invariant from §6 is currently guarded as noted. Per
ADR-0003 the Verus proofs are deferred; the guard today is an executable property
or round-trip test against the shipping code, which is exactly the adversarial
battery §6 describes running against "the executable code".

| Invariant | Meaning | Where | Current guard |
|-----------|---------|-------|---------------|
| I1b | Diff faithfulness: `apply(a, diff(a,b)) == b` | `omoplata-algebra` | Property test (round-trip) |
| I5 | Commutation soundness: commuting patches yield the same tree in either order | `omoplata-algebra` | Property test |
| I6 | Supersession well-formedness: the change graph is acyclic, no orphaned obsolescence | `omoplata-identity` | Unit/graph-invariant tests |
| I7 | Op-log invertibility: `undo ∘ op ≡ identity` on repository state | `omoplata-work` | Property/unit tests |
| I9 | Git round-trip fidelity: `export(import(x)) ≡ x` bit-identically | `omoplata-git` | Round-trip gate (tested, not proven — as designed) |
| I11 | Trivia conservation: merged comment tokens equal the union of both sides modulo base | `omoplata-drivers` | Structural-merge tests |

## Reductions from the design doc in this build

This build scaffolds the design doc's core faithfully but stands several external
systems and the formal-proof layer in with honest reductions. Read this section as
the definitive statement of what is *not* yet the real thing:

- **Verus formal proofs → property tests (ADR-0003).** The soundness core (I1a,
  I1b, I5, I6, I7, I8, I11, I12) is documented as proof obligations and guarded by
  property tests against the executable code, not machine-checked Verus theorems.
  The design doc's central claim — a *proven* kernel — is therefore approximated,
  not delivered.
- **Mergiraf fallback → built-in line/diff3 driver (ADR-0004).** Tier-2 structural
  merge is implemented for Rust; everything else falls back to a built-in
  line/diff3 driver rather than the Mergiraf adapter the doc names.
- **Real embedding model → deterministic hashing stand-in (ADR-0006).** The
  semantic layer (`dup`, `similar`) uses a deterministic hashing embedder behind a
  pluggable `Embedder` trait. It is good enough to demonstrate duplicate-work
  detection deterministically, but it is not a real transformer embedding model.
- **AletheiaDB substrate → loose-object store (ADR-0002).** The object store is a
  git-style loose-object directory, not the bi-temporal AletheiaDB graph the doc
  assumes as the substrate (P7).

**Genuinely not yet implemented from the design doc:**

- The **I8 runtime kernel-admission check** (every merge result carries a checked
  commutation witness or is a Conflict value) is not hosted as a single enforced
  boundary the drivers pass through — future work.
- Git **commit-graph / annotated-tag import**, the **wire protocol**, and
  **exact-mode export** (the full `export` half of I9 beyond the round-trip codec
  gate) are v1 future work; today's git leg is verify + blob/tree import.
- **Conflicts-as-values propagation through rebases** and **auto-rebase** (P3/P4
  beyond the op-log undo) are modeled in the algebra but not wired as a working
  rebase loop.
- **Dynamic validation (P9)** — demoting a kernel-accepted merge to a semantic
  conflict on CI build/test failure — is not implemented.
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
