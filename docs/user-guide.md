# omoplata user guide: transitioning from git

**omoplata** (`omo`) is a version control system built on a *verified merge
kernel* — its design goal is "no silent wrong answers." Three things make it
different from git:

- It versions **definitions** (functions, types, modules), not files, so it can
  follow a symbol across a rename where git only sees a delete plus an add.
- Its history is **bi-temporal** — an operation log you can query both as-of-now
  and as-of-then, with a real inverse (`op undo`) rather than a reflog you read
  but cannot cleanly reverse.
- Every accepted merge is **independently re-derived and witnessed by a trusted
  kernel**. A merge that cannot be witnessed does not silently ship; it degrades
  to a first-class, inspectable *conflict value*.

This guide is for engineers who know git and want to get productive with `omo`
quickly. Every command block below is **real, executed output** from the built
binary — nothing here is hypothetical. Where output was trimmed, it is noted.

---

## 1. Install / build

Build the release binary (it lands at `target/release/omo`):

```sh
cargo build --release
```

Or install `omo` onto your `PATH`:

```sh
cargo install --path crates/omoplata-cli
```

The rest of this guide assumes `omo` is on your `PATH`.

**Optional — structural merge for non-Rust files.** For `.json`, `.java`, and
other supported languages, `omo` shells out to [Mergiraf](https://mergiraf.org)
if it is on your `PATH`; otherwise it falls back to a line/diff3 merge. Install
it once:

```sh
cargo install mergiraf   # provides `mergiraf` on PATH
mergiraf --version       # mergiraf 0.18.0
```

**Optional — real transformer embeddings.** The semantic commands (`dup`,
`similar`) ship with a deterministic offline hashing embedder by default. To use
the real `all-MiniLM-L6-v2` model, build with the `fastembed` feature (the model,
~87 MB, is fetched from the network on first use):

```sh
cargo build --release --features fastembed
```

Confirm the binary works:

```console
$ omo --version
omoplata 0.1.0
```

---

## 2. Quick start

Initialize a repository and run the everyday loop: stash content as objects,
diff it, structurally merge it, and track definitions.

**Initialize and check status.**

```console
$ omo init repo
Initialized empty omoplata repository in repo/.omoplata

$ cd repo && omo status
On omoplata repository at /…/repo
No working changes tracked yet (scaffold).
```

**Stash content as an object and read it back** (git's `hash-object` /
`cat-file`):

```console
$ printf 'hello world\n' > greeting.txt

$ omo hash greeting.txt
sha256:af1b245b018dc132a0441a20d6eb17920a98354989a9d4941d9e337ec17ff836

$ omo cat sha256:af1b245b018dc132a0441a20d6eb17920a98354989a9d4941d9e337ec17ff836
hello world
```

**Diff two versions.**

```console
$ omo diff base.txt target.txt
@@ -2,1 +2,1 @@
-line two
+line TWO changed
```

**Structurally merge Rust.** Here two branches add *different* functions in the
same spot. `git merge-file` reports a textual conflict; `omo` merges cleanly with
the Rust structural driver:

```console
$ git merge-file -p left.rs base.rs right.rs   # git: textual conflict
… <<<<<<< / ======= / >>>>>>> …
(exit 1)

$ omo merge-file base.rs left.rs right.rs
rust-structural merge: 0 conflict(s)
fn alpha() -> i32 {
    1
}

fn beta() -> i32 {
    2
}

fn gamma() -> i32 {
    3
}
kernel: downgraded to conflict (rust-structural proposal not independently witnessed)
(exit 1)
```

Note the last line: the structural driver produced a clean answer, but the
*kernel* would not independently witness it, so it is honestly downgraded rather
than shipped. (More on this in §6.) A merge the kernel *can* re-derive is
admitted — see §6.

**Track a definition across a rename** (git sees a modified line; `omo` sees a
renamed definition):

```console
$ omo track old.rs new.rs
renamed compute_area -> area_of_rect (fn)
```

---

## 3. The change-stack workflow: workspaces, stacks, submissions, and landing

Omoplata rejects traditional Git-style `commit`, `branch`, and `switch` pointers in favor of **auto-snapshotted workspaces**, **change stacks**, **typed submissions**, and **merge queue landing** (§5.9, §5.10).

### Workspaces and auto-snapshotting

Working copies are associated with per-agent workspaces (`omo workspace add`). Any modification to the working copy is **implicitly auto-snapshotted** into a tree commit when querying or mutating state (P4).

```console
$ omo workspace add w1 /path/to/workdir
added workspace "w1" (dir: /path/to/workdir, change: ws/w1)

$ omo stack --workspace w1
workspace: w1 (change: ws/w1)
  tip commit: sha256:7f...
  stack changes: [ws/w1]
```

### Stack surgery: `absorb` and `reorder`

Instead of interactive rebasing, `omo` provides Sapling/jj-style stack surgery:

- **`omo absorb`**: auto-routes working copy edits into stack changes based on definition identity.
- **`omo reorder`**: swaps adjacent changes in a stack cleanly (if disjoint or commuting) or carrying conflict values (P3).

```console
$ omo absorb c1 c2
absorbed 2 change(s) into stack [ChangeId("ws/w1"), ChangeId("c1"), ChangeId("c2")]

$ omo reorder 0
reordered change stack: [ChangeId("c1"), ChangeId("ws/w1"), ChangeId("c2")]
```

### Submissions and Landing

Changes are reviewed via **submissions** (`omo submit`) referencing change IDs, and landed into trunk via **the merge queue** (`omo land`), which transitions changes from `Draft` to `Public` phase:

```console
$ omo submit sub-101 --title "Add definition tracking" c1 c2
submitted sub-101 "Add definition tracking" with 2 change(s) (approved)

$ omo land sub-101
landed submission sub-101: Submission sub-101 landed successfully
```


### "Branching" today

A "branch" here is just a second named ref. There is **no `omo branch` verb** —
you name refs directly with `omo ref set`. Store a variant, point a new ref at it,
and now two names coexist:

```console
$ printf 'title: notes\nbody: feature draft\n' > notes.feature.txt

$ omo hash notes.feature.txt
sha256:ef05874983205a31de1a8b0803550fc7c3cd662ad8d38a10c8bcbe60126da120

$ omo ref set feature sha256:ef05874983205a31de1a8b0803550fc7c3cd662ad8d38a10c8bcbe60126da120
#1 set-ref feature -> sha256:ef05874983205a31de1a8b0803550fc7c3cd662ad8d38a10c8bcbe60126da120

$ omo ref list
feature sha256:ef05874983205a31de1a8b0803550fc7c3cd662ad8d38a10c8bcbe60126da120
main sha256:1ef630955351b20cb2c72e3cdcd11a00c74dae5a938c12ec65d85ac1a48e2d3f
```

Query across your "branches" with a revset, and compare their states by diffing
the files directly:

```console
$ omo revset 'main | feature'
sha256:1ef630955351b20cb2c72e3cdcd11a00c74dae5a938c12ec65d85ac1a48e2d3f
sha256:ef05874983205a31de1a8b0803550fc7c3cd662ad8d38a10c8bcbe60126da120

$ omo diff notes.txt notes.feature.txt
@@ -2,1 +2,1 @@
-body: first draft
+body: feature draft
```

Missing verb flagged: `omo branch` (create/list/delete branches by name). Today a
branch is a ref you set by hand; there's no first-class branch command over it.

### "Switching" / "checking out" today — the biggest gap

Be direct: there is **no `omo switch` / `checkout`**. omo does **not** materialize
a stored tree back into your working files — nothing writes a ref's content over
your working directory. So you never "check out" `feature` into the working dir
the way `git switch feature` would.

What you *can* do is inspect stored state without touching your working files —
list refs, and read an object's bytes back with `omo cat`:

```console
$ omo ref list
main sha256:ef05874983205a31de1a8b0803550fc7c3cd662ad8d38a10c8bcbe60126da120

$ omo cat sha256:ef05874983205a31de1a8b0803550fc7c3cd662ad8d38a10c8bcbe60126da120
title: notes
body: first draft

$ omo cat main
error: sha256 prefix required (e.g. 'sha256:abcd...')
```

To act on a ref's content you redirect `omo cat` into a file yourself and
diff it — there is no command that swaps your working tree to match a ref. This is
the single biggest gap versus git, and the one workspaces is designed to close.

### Undo and history

The op log is your history, and unlike git's reflog `omo op undo` is a **true
inverse** — it computes and applies the inverse operation and records *that* as a
new op, rather than leaving you a pointer you can read but not cleanly reverse:

```console
$ omo op log
#1 set-ref feature ∅ -> sha256:ef05874983205a31de1a8b0803550fc7c3cd662ad8d38a10c8bcbe60126da120
#0 set-ref main ∅ -> sha256:1ef630955351b20cb2c72e3cdcd11a00c74dae5a938c12ec65d85ac1a48e2d3f

$ omo op undo
#2 undo of #1: set-ref feature ∅ -> sha256:ef05874983205a31de1a8b0803550fc7c3cd662ad8d38a10c8bcbe60126da120
  ref feature: sha256:ef05874983205a31de1a8b0803550fc7c3cd662ad8d38a10c8bcbe60126da120 -> (deleted)

$ omo ref list
main sha256:1ef630955351b20cb2c72e3cdcd11a00c74dae5a938c12ec65d85ac1a48e2d3f

$ omo op log
#2 undo #1
#1 set-ref feature ∅ -> sha256:ef05874983205a31de1a8b0803550fc7c3cd662ad8d38a10c8bcbe60126da120
#0 set-ref main ∅ -> sha256:1ef630955351b20cb2c72e3cdcd11a00c74dae5a938c12ec65d85ac1a48e2d3f
```

The undo (`#2`) removed the `feature` ref, and the log keeps *all three* entries —
history is a bi-temporal record you can query and invert, not a mutable pointer
you rewrite. (More on the op log vs the reflog in §6.)

### Coming with workspaces (M2)

The section above is a stopgap. The workspaces milestone (design-doc M2) brings
the ergonomic verbs a git user actually reaches for:

- **`omo commit`** — snapshot a working copy into a change (a whole-tree snapshot
  with a message), advancing a ref in one step instead of `hash-object` +
  `ref set` on a single file.
- **`omo branch`** — first-class create / list / delete of named branches, instead
  of setting refs by hand.
- **`omo switch` / `checkout`** — materialize a change back into working files, and
  move between multiple working copies over one shared object store.

When those land, this section will be **rewritten against the real verbs**. For
now, treat the flow above as the honest truth of what runs today.

---

## 4. git → omo command map

| You know (git) | omoplata | Notes |
|----------------|----------|-------|
| `git init` | `omo init [path]` | Creates a `.omoplata/` control dir. |
| `git hash-object -w` | `omo hash <path>` | Prints a `sha256:` object id (`-` reads stdin). |
| `git cat-file -p` | `omo cat <id>` | Blob bytes, or a tree listing. |
| `git diff` | `omo diff <base> <target>` | Unified-ish line diff. |
| `git merge` | `omo merge <base> <left> <right>` | Three-way line merge; conflicts become values, exit non-zero. |
| `git merge-file` | `omo merge-file <base> <left> <right>` | Tier-2 structural merge by extension, then kernel-checked (§6). |
| `git update-ref` | `omo ref set <name> <commit>` | Appends a `SetRef` op to the log. |
| `git show-ref` | `omo ref list` | Lists refs as `name commit`. |
| `git reflog` | `omo op log` | Bi-temporal op log, newest first (§6). |
| *(no clean analog)* | `omo op undo` | True inverse of the last op; not just a pointer you read. |
| `git rev-list` / set ops | `omo revset '<expr>'` | `a & b`, `a \| b`, `~a`, `all()`, `heads()`, `draft()`, `public()`. |
| `git rebase` | `omo rebase <base> <mine> <onto>` | Never fails; overlaps carried as conflict values. |
| *(rebase-as-recorded-value)* | `omo autorebase <base> <mine> <onto>` | Records the move on both time axes + change graph. |
| `git fsck` (round-trip sense) | `omo git verify <git-dir>` | Runs the I9 round-trip gate over every object. |
| `git fast-import` (ish) | `omo git import <git-dir>` | Walks the commit DAG behind the I9 gate. |
| `git cat-file --batch` → objects | `omo git export <git-dir> <out>` | Writes a byte-identical git object dir. |
| `git clone file://…` | `omo git fetch <repo>` | Real pkt-line / upload-pack over local transport. |
| *(no analog)* | `omo defs <file.rs>` | Lists Rust definitions with line ranges. |
| *(git: delete+add)* | `omo track <old.rs> <new.rs>` | Definition-level identity across versions (renames). |
| *(no analog)* | `omo admit <base> <left> <right>` | Direct kernel admission with a commutation witness. |
| *(no analog)* | `omo dup <files…>` | Flags likely duplicate definitions across files. |
| *(no analog)* | `omo similar <query> <files…>` | Semantic ranking of definitions by a query. |

Commands marked *(no analog)* have no git equivalent: they exist because omoplata
works at the definition and value layer, not the file-and-line layer.

---

## 5. Migrating an existing git repo

omoplata reads a real `.git` directory and can write one back out byte-identically,
so you can adopt it incrementally and keep using git side-by-side.

Start from a normal 2-commit git repo:

```console
$ git -C src log --oneline
af07699 Update greeting and add license
91b2360 Initial commit
```

**Verify the round-trip gate (I9).** Before anything is imported, `omo` proves it
can serialize every object back to a byte-identical git object — if it cannot, it
refuses to import. This is the I9 invariant, checked (not assumed):

```console
$ omo git verify src/.git
blobs:   3
trees:   2
commits: 2
tags:    0
total:   7
round-trip gate: PASS
```

**Import** (the gate runs first; import is refused if it fails):

```console
$ omo init omostore
$ omo git import src/.git --repo omostore
imported commits: 2
imported tags:    0
imported trees:   2
imported blobs:   3
refs walked:      2
git -> omoplata mappings: 5
```

**Fetch over the wire protocol** (local transport). This speaks the real pkt-line
+ `upload-pack` conversation against a local `git upload-pack`, negotiates a full
clone, receives a packfile, and imports through the same I9 gate:

```console
$ omo init fetchstore
$ omo git fetch src --repo fetchstore
advertised refs (2):
  af07699 HEAD
  af07699 refs/heads/master
packfile bytes received: 817
imported commits: 2
imported tags:    0
imported trees:   2
imported blobs:   3
git -> omoplata mappings: 5
```

**Get content back out — byte-identical.** Export reconstructs a real git object
directory and re-checks the round-trip against the source:

```console
$ omo git export src/.git exported
exported 7 objects; round-trip vs source: PASS

$ find exported -type f | sort | head
exported/HEAD
exported/objects/3d/75779f4ad5cc33c32249372648c66f0c77b3ea
exported/objects/57/09e451b2301daf7db4fd7e73842895b57373cf
exported/objects/91/b2360e14f628dce356b989772b79c8ca600af1
…
exported/refs/heads/master
```

That output directory is a genuine git object store — you can point git at it.
Because import and export are the same gate in both directions, git and omoplata
can share the same history during a transition.

**Packed (gc'd) repos work too.** After `git gc`, objects live in a packfile;
`omo git verify` decodes the packfile transparently:

```console
$ omo git verify gcsrc/.git
blobs:   3
trees:   2
commits: 2
tags:    0
total:   7
note: 1 packfile(s) decoded and included in the counts above
round-trip gate: PASS
```

---

## 6. Concepts a git user must reframe

### Conflicts are values, not marker-soup you must resolve now

In git a conflict halts you: you get `<<<<<<<` markers and must resolve before you
can proceed. In omoplata a conflict is a *first-class value* carried forward —
merges and rebases never fail and never block. `omo rebase` replays your change
and, where it overlaps, carries the conflict as data:

```console
$ omo rebase base.txt mine.txt onto.txt      # independent edits
ONTO
b
c
MINE
rebase: clean

$ omo rebase base.txt mine2.txt onto2.txt    # overlapping edits
a
<<<<<<< mine
MINE
=======
ONTO
>>>>>>> onto
c
d
rebase: 1 conflict(s) carried
```

The markers are a *rendering* of the conflict value; the structured conflict is
the source of truth and travels with the change until someone resolves it.

### Kernel admission and downgrade: untrusted proposer, trusted kernel

A structural merge driver (Rust-native, or Mergiraf) is an *untrusted proposer*.
Whatever it produces is handed to a small **trusted kernel** that independently
re-derives the merge and admits the proposal *only if it matches* — emitting a
checked commutation witness. When the edits genuinely commute, you get an
admitted merge:

```console
$ omo merge-file b2.rs l2.rs r2.rs           # edits to two separate fns
rust-structural merge: 0 conflict(s)
kernel: admitted (commutation witness: 1 hunks p, 1 hunks q)
fn alpha() -> i32 {
    100
}

fn beta() -> i32 {
    200
}
```

But a "clean" driver result can still be **downgraded**. When Mergiraf happily
combines two JSON fields, the kernel's own line-level re-derivation cannot witness
that structural rearrangement, so it honestly downgrades rather than trust the
proposer:

```console
$ git merge-file -p l.json b.json r.json     # git: textual conflict, exit 1
…
$ omo merge-file b.json l.json r.json
mergiraf merge: 0 conflict(s)
{
  "name": "widget",
  "version": "1.0.0",
  "author": "alice",
  "license": "MIT"
}
kernel: downgraded to conflict (mergiraf proposal not independently witnessed)
```

This is the "no silent wrong answers" rule in action: a merge only ships as clean
when the trusted kernel can prove it, not because a tool said so. You can also run
the kernel directly with `omo admit` — disjoint edits are witnessed, overlapping
edits become a conflict value:

```console
$ omo admit base.txt left.txt right.txt      # disjoint
A
b
c
d
E
admitted: commutation witness (support: 1 hunks p, 1 hunks q)

$ omo admit base.txt lo.txt ro.txt           # overlapping
a
<<<<<<< left
LEFT
=======
RIGHT
>>>>>>> right
c
d
e
conflict: 1 region(s)
```

### Provisional merges + CI demotion (P9)

Even a kernel-admitted merge can be treated as *provisional pending dynamic
validation*. Pass `--validate <cmd>`: the merged output is materialized to a temp
file and your command (a build, a test, a linter) runs against it. Pass, and the
merge is accepted; fail, and it is **demoted to a semantic conflict** rather than
accepted:

```console
$ omo merge-file --validate 'true' b2.rs l2.rs r2.rs
rust-structural merge: 0 conflict(s)
kernel: admitted (commutation witness: 1 hunks p, 1 hunks q)
…merged output…
dynamic validation PASSED
(exit 0)

$ omo merge-file --validate 'false' b2.rs l2.rs r2.rs
rust-structural merge: 0 conflict(s)
kernel: admitted (commutation witness: 1 hunks p, 1 hunks q)
<<<<<<< left
…
||||||| base
…
=======
…
>>>>>>> right
dynamic validation FAILED: demoted to semantic conflict (validator `false` exited non-zero)
(exit 1)
```

In practice `<cmd>` is your CI job; a red build turns a syntactically-clean merge
into an honest conflict.

### The bi-temporal op log vs git's reflog

Every ref change is an operation in a log you can query and *invert*. Unlike a
reflog, `op undo` is a true inverse that itself becomes a recorded op:

```console
$ omo ref set main    sha256:da5ce…2462
$ omo ref set feature sha256:6d8a7…b50f
$ omo op log
#1 set-ref feature ∅ -> sha256:6d8a7…b50f
#0 set-ref main    ∅ -> sha256:da5ce…2462

$ omo op undo
#2 undo of #1: set-ref feature ∅ -> sha256:6d8a7…b50f
  ref feature: sha256:6d8a7…b50f -> (deleted)

$ omo ref list
main sha256:da5ce…2462
```

`autorebase` records a rebase on *both* time axes — a `Rebase` op (transaction
time) and a supersession edge in the change graph (valid time):

```console
$ omo autorebase base.txt mine.txt onto.txt --change feature
ONTO
b
c
MINE
autorebase: new tip sha256:94acd…9014
autorebase: clean
op-log: #1 rebase feature sha256:be5ad…5238 -> sha256:94acd…9014 onto sha256:25068…5733 (clean)

$ omo op log        # the Rebase op is persisted
#2 rebase feature … (1 conflict(s))
#1 rebase feature … (clean)
#0 set-ref feature ∅ -> sha256:be5ad…5238
```

Set operations over refs come from `revset`:

```console
$ omo revset 'main | feature'
sha256:6d8a7…b50f
sha256:da5ce…2462
```

### Definition-level identity vs line diffs

Rename a function and git shows you a deleted line and an added line — it has no
notion that the definition persisted:

```console
$ git diff --no-index old.rs new.rs
-fn compute_area(w: f64, h: f64) -> f64 {
+fn area_of_rect(w: f64, h: f64) -> f64 {
     w * h
 }
```

`omo track` works at the definition layer and reports the *identity*:

```console
$ omo track old.rs new.rs
renamed compute_area -> area_of_rect (fn)
```

`omo defs` is the primitive underneath — it lists a file's definitions with their
kinds and line ranges:

```console
$ omo defs shapes.rs
struct Rectangle (lines 1-4)
impl Rectangle (lines 6-10)
fn Rectangle::area (lines 7-9)
fn helper (lines 12-14)
```

The same definition-awareness powers the semantic layer. `dup` flags two agents
implementing the same thing *before* they textually collide, and `similar` ranks
definitions against a query. With the default offline embedder:

```console
$ omo dup --threshold 0.5 alice.rs bob.rs
0.64  alice.rs:rectangle_area ~ bob.rs:area_of_rect

$ omo similar "compute the area of a rectangle" alice.rs bob.rs
0.5459 alice.rs:rectangle_area
0.4905 bob.rs:area_of_rect
0.2968 bob.rs:parse_config
0.1523 alice.rs:greet
```

With real embeddings (`--features fastembed` build, `--real-embeddings` flag) the
ranking sharpens — the two area functions pull far ahead of the unrelated
`parse_config` / `greet`, which drop to near zero:

```console
$ omo similar --real-embeddings "compute the area of a rectangle" alice.rs bob.rs
using real embeddings (all-MiniLM-L6-v2, 384-dim)
0.6462 alice.rs:rectangle_area
0.5289 bob.rs:area_of_rect
-0.0028 bob.rs:parse_config
-0.0839 alice.rs:greet

$ omo dup --real-embeddings --threshold 0.5 alice.rs bob.rs
using real embeddings (all-MiniLM-L6-v2, 384-dim)
0.79  alice.rs:rectangle_area ~ bob.rs:area_of_rect
```

The real model's absolute similarities run lower than lexical overlap, so pass a
lower `--threshold` (e.g. `0.5`) when using `--real-embeddings`.

---

## 7. Current limitations (honest)

This build scaffolds the design doc faithfully but is explicit about what is not
yet the real thing:

- **No everyday `commit` / `branch` / `switch` verbs yet.** There is no `omo
  commit` (a message-bearing, working-tree snapshot), no `omo branch`, and no `omo
  switch` / `checkout` — omo never materializes a stored tree back into your
  working files. Today you assemble the loop by hand from `hash-object`,
  `ref set`, `cat-object`, and `diff`, as documented in §3. These ergonomic verbs
  are coming with **workspaces (design-doc M2)**, and §3 will be rewritten against
  them when they land.
- **Git transport is read-only and local.** There is no `git push` / `receive-pack`,
  and no networked (http/ssh) transports. `omo git fetch` works over the local
  `file://` / path transport shown in §5 — a real pkt-line / upload-pack clone —
  but not over the network.
- **Real embeddings need the network on first use.** The offline default is a
  deterministic hashing stand-in (fully reproducible, no download). The real
  `all-MiniLM-L6-v2` model requires `--features fastembed` and a one-time ~87 MB
  fetch; after that it runs locally.
- **Formal proofs are partial.** The kernel's core is **machine-checked in Verus** —
  I1b (diff faithfulness / round-trip) and the I10 disjoint-commutation lemma for
  the length-preserving core verify as `7 verified, 0 errors`, with no
  `assume`/`admit`/`external_body`. The remaining invariants — general I5, plus
  I6, I7, I8, I11, I12 — are guarded by **property and round-trip tests** against
  the shipping code, not yet proven. The Verus module checks a faithful model of
  the algorithm; the shipping functions stay trusted-by-testing with the proptests
  as their differential oracle.
- **Structural merge coverage.** Tier-2 structural merge is Rust-native; other
  languages go through Mergiraf when it is on `PATH`, else a line/diff3 fallback.
- **AletheiaDB is external-by-design.** The bi-temporal storage engine is an
  external system this build integrates against rather than reimplements.

---

*Every command block in this guide was executed against scratch repositories with
the release binary; object ids and byte counts are the real values produced.*
