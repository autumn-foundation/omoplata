# omoplata

**A version control system built on a verified merge kernel — *no silent wrong
answers*.** omoplata treats the *definition* (a function, type, or module), not
the file, as the unit of version control; records history that is bi-temporal and
queryable in both valid time (what was true) and transaction time (what was
believed); and reads and writes the git object format so it can be smuggled in as
a backend behind existing tooling. Every accepted merge is checked; everything
else degrades to an honest, first-class conflict.

> **Status: implementation in progress.** This repository implements the core of
> [`Omoplata_design_doc.md`](Omoplata_design_doc.md) end to end through the `omo`
> CLI — an 8-crate workspace covering the object store, patch algebra, definition
> identity, the bi-temporal operation log, Tier-2 merge drivers, git interop, and
> the semantic layer, plus the **working model** (§5.9: per-agent workspaces,
> change stacks, `absorb`/`reorder`) and the **review-and-landing layer** (§5.10,
> ADR-0009: submissions with approvals, named merge queues with per-queue policy,
> Tier-0 batch landing at definition granularity, and certified backports). It is
> honest about its reductions: the merge kernel's
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
| `omo hash [--repo DIR] <path>` | Store a file as a blob and print its `sha256:` id (`-` reads stdin). | `omo hash README.md` |
| `omo cat [--repo DIR] <id>` | Print a stored object: blob bytes, or a tree listing. | `omo cat sha256:…` |

### Workspaces and Change Stacks (§5.9)

| Command | Description | Example |
|---------|-------------|---------|
| `omo workspace add <name> <dir>` | Register a working copy directory for an agent/user workspace. | `omo workspace add w1 ./wc` |
| `omo workspace list` | List all active registered workspaces and their working directories. | `omo workspace list` |
| `omo workspace remove <name>` | Unregister a workspace. | `omo workspace remove w1` |
| `omo stack [--workspace WS]` | View linear change stack; auto-snapshots working copy modifications into tree commits (P4). | `omo stack --workspace w1` |
| `omo switch <target> [--workspace WS] [--force]` | Repoint a workspace at another change (a teammate's `ws/<name>`, a change id, or a landed change) and materialize its live tip into the working dir — the one-liner to switch to someone's work and pull in what landed since. Refuses to clobber uncommitted edits unless `--force`. | `omo switch ws/agent-2 --workspace agent-1` |
| `omo absorb <change...>` | Auto-route working copy edits to stack changes by definition identity. | `omo absorb c1 c2` |
| `omo reorder <index>` | Swap adjacent changes in a stack (carrying conflict values if non-disjoint). | `omo reorder 0` |

### Submissions and Merge Queue (§5.10)

| Command | Description | Example |
|---------|-------------|---------|
| `omo submit <id> --title "..." <change...>` | Create a review submission for change ID revsets with approval certificates; `--pending` leaves it awaiting review instead of auto-approving. | `omo submit sub-101 --title "Add auth" ws/w1` |
| `omo approve <id> [--by NAME]` | Approve a pending submission. | `omo approve sub-101 --by mark` |
| `omo land <id>... [--queue NAME]` | Land approved submission(s) through a named merge queue (default `trunk`), transitioning phase from `Draft` to `Public`. The queue's policy gates the landing (approval, carried conflict values, P9 validation). Landing **auto-reconciles** (ADR-0010 Phase 3): the content is structurally merged against the queue's pre-land base and the `reconciled/<queue>` merged trunk advances in the same transaction — two agents editing different definitions of one file both land and both survive; line-disjoint edits to the *same* definition merge; an incompatible pair rides through as a conflict value on a permissive queue, and a strict queue refuses to keep it. | `omo land sub-101 sub-102 --queue release-1.2` |
| `omo backport <id> --to <queue>` | Land an already-landed submission into a second queue, carrying its approval forward with a certificate — *identity* (content byte-identical to the reviewed tip) or *commutation* (moved, but every changed definition matches the source queue's landed history, so nothing reviewed was altered); an invented, unreviewed definition demands re-review. Target queue gates still apply. | `omo backport sub-101 --to release-1.2` |
| `omo queue add <name>` | Register a landing queue with its policy: `--validate CMD` (P9 validator, `{}` = content dir), `--allow-carried`, `--no-approval`, `--description`. Registered queues default strict (carried values refused); the implicit `trunk` is permissive. | `omo queue add release-1.2 --validate './regression.sh {}'` |
| `omo queue list` | List queues (including the implicit `trunk`) with their policies. | `omo queue list` |

### Remotes and distribution (ADR-0010, Phase 1)

| Command | Description | Example |
|---------|-------------|---------|
| `omo remote add <name> <path>` | Register another omoplata repository as a named remote (local-path transport). | `omo remote add origin ../peer` |
| `omo remote list` / `omo remote remove <name>` | List or drop registered remotes. | `omo remote list` |
| `omo fetch <remote>` | Replicate a remote's landed (`public/*`) state: copy the object closure into the local store and record each tip under `remotes/<name>/*` via the op log. Idempotent (content-addressed); private `ws/*` refs are not fetched. Then `omo switch <name>/<change>` drops onto a teammate's remote-landed work — no git round-trip. | `omo fetch origin` |
| `omo push <remote> <id> [--queue Q]` | Land a local submission on a remote through *its* landing policy (the remote is the authority, ADR-0010 Phase 2): replicate the submission's content, then run the remote's approval/carried/validator/disjointness gates against the remote's landed state and land under its lock — or refuse with the reason. Content is re-validated by the remote, not trusted. | `omo push origin sub-101 --queue release-1.2` |
| `omo reconcile <id...> [--queue Q]` | Reconcile *without* landing: fold submissions into one merged tree against a queue's landed base, carrying same-definition conflicts as **values** (§5.4) instead of refusing, and write the `reconciled/<queue>` head you can `omo switch` onto (ADR-0010 Phase 3). `omo land`/`omo push` do this automatically; use `reconcile` to preview or produce a merged trunk without landing. Exit 0 clean, 2 carrying values; a strict queue refuses to keep them. | `omo reconcile sub-a sub-b` |
| `omo queue remove <name>` | Remove a queue from the registry (landed refs are kept). | `omo queue remove release-1.2` |

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
| `omo merge-file <base> <left> <right>` | Tier-2 driver merge chosen by extension: `.rs` uses the Rust structural driver (definition granularity, recursing into `impl`/`mod`/`trait` members); supported non-Rust files use the Mergiraf shell-out when it is on `PATH`, else the line fallback. Inputs may carry conflict values (§5.4): they ride through untouched definitions (exit 2, `carried forward`) instead of degrading the merge. | `omo merge-file base.json left.json right.json` |
| `omo conflicts <file>` | List the conflict values a file carries, each pinned to the definition containing it. Exit 0 = none, 2 = values present. | `omo conflicts src/lib.rs` |

### History and revsets

| Command | Description | Example |
|---------|-------------|---------|
| `omo ref set <name> <commit> [--repo DIR]` | Point a ref at a commit (appends a `SetRef` op to the log). | `omo ref set main sha256:…` |
| `omo ref list [--repo DIR]` | List the current refs as `name commit`. | `omo ref list` |
| `omo op log [--repo DIR]` | Print the bi-temporal operation log, newest first. | `omo op log` |
| `omo op undo [--repo DIR]` | Undo the most recent operation still in effect (total, invertible undo). | `omo op undo` |
| `omo revset <expr> [--repo DIR]` | Evaluate a revset expression (`a & b`, `a \| b`, `~a`, `all()`, `heads()`, `draft()`, `public()`, `landed(<queue>)`, `id:<hex>`). `landed(release-1.2) & ~landed(trunk)` is the "needs backporting to trunk" query. | `omo revset 'landed(trunk) & ~landed(release-1.2)'` |

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
| `omo dup [file.rs]... [--threshold T]` | Flag likely duplicate definitions across active workspaces or specified files (convergent work before textual collision). | `omo dup` |
| `omo similar <query> <file.rs>... [--top K]` | Rank definitions by similarity to a free-text query. | `omo similar "area of rectangle" a.rs` |


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

**Since this section was first written, several items moved from "not yet" to
shipped** — recorded here so the reductions stay honest:

- **The working and landing layer is real, not scaffolded.** `omo workspace` /
  `stack` / `absorb` / `reorder` (§5.9) and `omo submit` / `approve` / `land` /
  `backport` / `queue` (§5.10, ADR-0009) drive the change-graph end to end:
  submissions carry approvals, landing is the `Draft → Public` phase transition,
  named queues gate on per-queue policy (P9 validator, approval, carried-conflict
  rule), `omo land a b c` batches **pairwise-disjoint changes at definition
  granularity** (two agents editing different definitions of one file batch),
  and `omo backport` carries approval forward under an identity certificate.
- **Conflicts-as-values propagate through the merge path** (§5.4, P3): the Rust
  structural driver merges at definition granularity **recursing into
  `impl`/`mod`/`trait` members**, carries pre-existing conflict values through
  untouched definitions (`omo merge-file` reports them, exits 2), and
  `omo conflicts <file>` lists them pinned to their definitions. `omo rebase` /
  `omo autorebase` carry conflicts as values through the change graph and op log
  rather than blocking.
- **Kernel admission is a live boundary:** `omo merge-file` passes every clean
  driver proposal through `kernel::certify`, downgrading to a Conflict when the
  kernel cannot independently witness it; `omo admit` runs the kernel directly.

**Genuinely still reduced or not implemented from the design doc:**

- **Landing is immediate, not a persistent queue.** `land` applies policy and
  transitions synchronously; there is no standing queue membership, `queued()`
  revset, or single-writer landing daemon (ADR-0008 Option C) yet — the daemon
  is the documented destination that internalizes today's per-command lock.
- **Approvals are single-reviewer** (`require_approval: bool`); multi-approval
  thresholds and named-reviewer policies await the bi-temporal approval model
  (§5.6). **Batch disjointness and backport certificates use identity/support
  witnesses**, not the general I5 commutation certificate (future work, ADR-0009).
- The **I8 kernel-admission check** is hosted at `merge-file`/`admit`, but is not
  yet threaded as a mandatory gate through the landing path or every driver call
  site — future work.
- Git **packfile/delta decoding** (index v2, packfile v2/v3, `OFS_DELTA`, `REF_DELTA`) and **wire-protocol fetch over local transport** (`omo git fetch`) are implemented, closing the I9 `import → export → bit-identical` loop across both loose and packed objects. Outstanding git future work is **push (`receive-pack`)** (which requires a packfile encoder) and **networked (`http`/`ssh`) socket transports** (not offline-testable).
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
