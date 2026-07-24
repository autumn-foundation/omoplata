# `omo` command reference

Exhaustive command list, flags, and exit-code semantics. Read this when the
SKILL.md playbook does not cover a flag or edge case you need. Commands that
operate on a repository take `--repo DIR` (default: current directory);
`init`/`status` take a positional path instead.

## Exit-code convention

`omo` returns:

- **0** — success / clean result.
- **1** — an honest first-class conflict (`merge`, `merge-file`, `rebase`): the
  output carries `<<<<<<<` markers and the structured conflict is the source of
  truth.
- **2** — either (a) a **carried conflict value** state (`merge-file`,
  `conflicts`: a *landable* result that carries unresolved values from the
  inputs), or (b) any **hard error** (bad args, refused landing, missing object).
  These share the code — disambiguate by reading `stderr`.

Merged/derived text goes to **stdout**; summaries, kernel verdicts, and gate
outcomes go to **stderr**.

## Repository and objects

| Command | What it does |
|---|---|
| `omo init [path]` | Create a repository (a `.omoplata/` control dir). |
| `omo status [path]` | Report whether a directory is an initialized repo. |
| `omo hash [--repo DIR] <path>` | Store a file as a blob, print its `sha256:` id (`-` = stdin). |
| `omo cat [--repo DIR] <id>` | Print a stored object: blob bytes or a tree listing. Ids need the `sha256:` prefix. |

## Workspaces and change stacks (§5.9)

| Command | What it does |
|---|---|
| `omo workspace add <name> <dir> [--repo DIR]` | Register a working directory + mint its change `ws/<name>`. Creates the dir if absent. |
| `omo workspace list [--repo DIR]` | List workspaces: `name  <dir>  change=<id>  tip=<commit>`. |
| `omo workspace remove <name> [--repo DIR]` | Unregister a workspace (op-log history is kept). |
| `omo stack [--workspace WS] [--repo DIR]` | View the workspace's change stack; **auto-snapshots** a dirty working copy into a tree commit (P4). |
| `omo absorb <change...> [--workspace WS] [--repo DIR]` | Route working-copy edits into stack changes by touched definition identity (§5.9). |
| `omo reorder <index> [--workspace WS] [--repo DIR]` | Swap adjacent stack changes (disjoint/commuting swap cleanly; else a conflict value is carried). |

Workspaces share one object store, op log, and refs. Concurrent `omo` processes
serialize on an advisory `flock` and write the op log atomically (ADR-0008), so
many agents can drive one repo safely.

## Submissions, approvals, landing, queues (§5.10, ADR-0009)

| Command | What it does |
|---|---|
| `omo submit <id> --title <t> <change...> [--author A] [--pending] [--repo DIR]` | Create a submission over change IDs. Auto-approves unless `--pending`. |
| `omo approve <id> [--by NAME] [--repo DIR]` | Approve a pending submission. |
| `omo land <id>... [--queue NAME] [--repo DIR]` | Land one or more submissions through a queue (default `trunk`), transitioning `Draft → Public`. Multiple ids = a **Tier-0 batch**. |
| `omo backport <id> --to <queue> [--repo DIR]` | Land an already-landed submission into a second queue, carrying approval forward under an *identity* certificate (content unchanged) or a *commutation* certificate (moved, but every changed definition matches the source queue's landed history); an invented, unreviewed definition refuses pending re-review. |
| `omo queue add <name> [--validate CMD] [--allow-carried] [--no-approval] [--description D] [--repo DIR]` | Register a landing queue with its policy. Registered queues default strict (approval required, carried conflicts refused). |
| `omo queue list [--repo DIR]` | List queues (including implicit `trunk`) with their policies. |
| `omo queue remove <name> [--repo DIR]` | Remove a queue (landed refs are kept). |

### Landing gate order (all checked before any state changes)

1. **Approval** — required unless the queue policy waives it (`--no-approval`).
2. **Carried conflict values** — a strict queue refuses content still carrying
   §5.4 values; `trunk` (and `--allow-carried` queues) accept them.
3. **P9 validation** — if the queue has `--validate CMD`, the submission's
   materialized content is checked and only a pass lands. `{}` in `CMD` is
   replaced with the content directory (appended if absent).

A refused landing (any gate) mutates nothing and exits 2 with an explanatory
`stderr`.

### Batch landing (definition-granular Tier-0)

`omo land a b c` batches when the submissions are **pairwise-disjoint at
definition granularity** relative to the queue's current landed state: a
submission's *support* for a file is the set of definitions it changed vs the
landed base. `impl`/`mod`/`trait` containers compare by their shell (members
elided), so adding different methods to one `impl` is disjoint. Overlap on a
shared definition refuses the **whole** batch, naming the definition. Disjoint
support licenses order-independence (I3′): the batch validates as one and lands
in a single locked transaction.

### Per-queue refs

Trunk landings write `public/<change>` (legacy shape); every other queue writes
`public/<queue>/<change>`. The same change can therefore land in several queues
(the backport story) without forking identity.

## Merge, kernel, conflicts

| Command | What it does |
|---|---|
| `omo diff <base> <target>` | Line diff turning `base` into `target`. |
| `omo merge <base> <left> <right>` | Three-way **line** merge; conflicts render as markers, exit 1. |
| `omo merge-file <base> <left> <right> [--validate CMD]` | Tier-2 driver merge by extension (`.rs` → structural). Clean proposals pass through `kernel::certify`; a non-witnessed proposal downgrades to a conflict. Exit 0 clean+admitted, 1 conflict/downgrade, 2 carried-forward. `--validate` demotes a build/test failure to a semantic conflict (P9). |
| `omo admit <base> <left> <right>` | Run the trusted kernel directly (no proposer): admitted merge with a commutation witness (exit 0), or first-class conflict (exit non-zero). |
| `omo conflicts <file>` | List conflict values a file carries, each pinned to its definition. Exit 0 none, 2 some. |
| `omo rebase <base> <mine> <onto>` | Replay `mine` onto `onto`; overlaps carried as conflict values (never fails). |
| `omo autorebase <base> <mine> <onto> [--change C] [--repo DIR]` | Auto-rebase through the change graph + op log (records supersession + a Rebase op). |

The structural driver is **Rust-only**; definition-granularity behavior
(clean disjoint merges, member recursion, batch disjointness) applies to `.rs`.

## Definitions and identity

| Command | What it does |
|---|---|
| `omo defs <file.rs>` | List a file's Rust definitions as `<kind> <path> (lines A-B)`, source order. |
| `omo track <old.rs> <new.rs>` | Report definition identity across two versions: added / deleted / renamed / modified / unchanged. |

## History, refs, revsets (§5.6, §5.8)

| Command | What it does |
|---|---|
| `omo ref set <name> <commit> [--repo DIR]` | Point a ref at a commit (appends a `SetRef` op). |
| `omo ref list [--repo DIR]` | List refs as `name commit`. |
| `omo op log [--repo DIR]` | Print the bi-temporal op log, newest first. |
| `omo op undo [--repo DIR]` | Apply the inverse of the last op and record *that* as a new op (a true inverse, not a reflog pointer). |
| `omo revset <expr> [--repo DIR]` | Evaluate a revset over refs. |

### Revset language

Set algebra over commits: `a & b` (intersection), `a | b` (union), `~a`
(complement), parentheses. `~` binds tightest, then `&`, then `|`.

Atoms and functions:

- a bare ref name (`main`), or `id:<hex>` for a specific commit
- `all()`, `heads()`, `draft()`, `public()`
- `landed(<queue>)` — commits landed in a queue; bare `landed()` means `trunk`.
  Ref-namespace disambiguation is registry-driven: a `public/…` ref belongs to a
  non-trunk queue iff its first segment names a registered queue.

Canonical query — **what needs backporting from trunk to a release line**:

```sh
omo revset 'landed(trunk) & ~landed(release-1.2)'
```

## Semantic layer (§5.7)

| Command | What it does |
|---|---|
| `omo dup <files...> [--threshold T] [--real-embeddings] [--repo DIR]` | Flag likely duplicate work across Rust files by embedding similarity. Empty file list scans all registered workspaces. |
| `omo similar <query> <files...> [--real-embeddings]` | Rank definitions across the given Rust files by similarity to `<query>`, printing the top-k as `<score> <file>:<def>`. |

Default embeddings are a deterministic offline hashing stand-in; `--real-embeddings`
(needs `--features fastembed` at build) uses `all-MiniLM-L6-v2` — pass a lower
`--threshold` (~0.5) with it, since real-model similarities run lower than
lexical overlap.

## Git interop (§3 P8, I9)

`omo git <subcommand>` — the round-trip gate (`export(import(x)) == x`) and
import. Fetch works over local `file://`/path transport with full packfile/delta
decoding. Push (`receive-pack`) and networked (http/ssh) transports are not yet
implemented.
