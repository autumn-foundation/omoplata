---
name: omoplata
description: >-
  Use whenever a task involves omoplata or its `omo` CLI — a standalone version
  control system that is NOT git, recognized by the word "omoplata", an `omo`
  command, or a `.omoplata/` directory in the project. Trigger for anything in
  that world: getting edited files into review, submitting/approving/landing/
  backporting changes, merge queues, release lines, batch landing, structural
  file merges (`merge-file`), resolving or carrying conflicts, or writing
  revsets. Trigger especially for multi-agent or swarm work over one shared
  omoplata repo — one workspace per agent, submitting and landing independently
  without clobbering. Also trigger to interpret `omo` output: exit codes,
  "carried forward" and "not approved" messages, conflict values. Any land/
  submit/merge/backport/queue/workspace/revset task in an omoplata context
  belongs here, even if `omo` is never typed. Do NOT trigger for git, GitHub,
  darcs, or generic release-planning equivalents.
---

# Driving omoplata (`omo`)

omoplata is a version control system built on a **verified merge kernel** — its
guarantee is *no silent wrong answers*. It is not git with different verbs. Four
mental shifts decide whether you use it well:

1. **The unit of history is the definition, not the file.** Merges, identity, and
   batch-disjointness are computed per *definition* (function, type, `impl`
   member), not per line. Two agents editing different functions of one file do
   not conflict.
2. **Conflicts are values, not a stop-the-world event.** A merge or rebase never
   fails and never blocks. An unresolved conflict rides through as a first-class
   value you resolve *later*, when convenient. Landing throughput never waits on
   resolution.
3. **There are no branches.** The primary objects are **changes** and **stacks**.
   You do not `git branch` / `commit`. You register a **workspace**, edit files,
   and the working copy **auto-snapshots** into a change; to move a workspace onto
   another change you `omo switch`, not `git checkout`.
4. **Landing is a policy gate, not a push.** Work reaches trunk (or a release
   line) by `submit` → `approve` → `land` through a **merge queue** whose policy
   (validator, approval, conflict rules) is checked *before* the change goes
   public — in-band, not after a merge commit exists.

Build the binary if it is not on `PATH`: `cargo build --release` →
`target/release/omo`. Every repo-scoped command takes `--repo DIR` (default: cwd).
For the full command/flag/exit-code reference, read
[`references/commands.md`](references/commands.md).

## The core loop (single agent)

```sh
omo init myrepo                                  # create .omoplata/
omo workspace add w1 ./wc --repo myrepo          # register a working dir + change ws/w1
# ...edit files in ./wc...
omo stack --workspace w1 --repo myrepo           # auto-snapshots dirty working copy into the change
omo submit sub-1 --title "Add auth" ws/w1 --repo myrepo   # create a review submission
omo land sub-1 --repo myrepo                     # land through the trunk queue (Draft -> Public)
```

`submit` auto-approves by default. Pass `--pending` to leave it for review, then
`omo approve sub-1 --by <name>` before landing — required by any queue whose
policy demands approval.

## Multi-agent / swarm development — the payoff

This is what omoplata is *for*. Give each agent its own workspace over **one
shared repo** — no clones, no worktrees, no push/rebase retry loops. Every `omo`
process serializes on an advisory lock and writes atomically, so N agents land
concurrently without corrupting the op log.

**Across machines (distributed, ADR-0010 Phase 1):** the shared repo is the
local model; to work against another dev's repo, register it and replicate its
landed state — no git import/export round-trip:

```sh
omo serve --addr 127.0.0.1:9000 --repo trunk   # (on the host) landing authority over HTTP
omo remote add origin http://host:9000  # or a filesystem path for the local-path transport
omo fetch origin                      # copy its public/* into remotes/origin/*
omo switch origin/agent-2 --workspace me   # drop onto their remote-landed work
omo push origin sub-7 --queue trunk   # land YOUR submission on the remote's queue
```

A remote is a filesystem path (shared mount / sibling clone) *or* an
`http://host:port` URL of an `omo serve` daemon — same commands, same semantics.
The HTTP transport takes an **optional bearer token** (`omo serve --token <t>`;
clients pass `omo remote add --token <t>` or set `OMO_TOKEN`) — a missing/wrong
token gets `401`. **TLS is not built in**: front it with a proxy/tunnel and point
`omo` at the plaintext side (a token on a bare HTTP hop only makes sense behind
one, or on a trusted network).

`fetch` is read-only replication of *landed* (`public/*`) state (private `ws/*`
tips are not fetched — land to share). `push` is the write path: the **remote is
the landing authority** — it re-runs its own approval/validator/disjointness
gates against its own trunk and lands under its lock, or refuses. The client
can't bypass the remote's policy.

When two agents change the **same file**, don't reach for a rebase — reconcile:

```sh
omo reconcile sub-a sub-b            # merge both against the queue base into one tree
```

Different definitions of one file combine cleanly; edits to the *same* definition
ride through as **conflict values** (§5.4), not a refusal — where git rejects a
non-fast-forward, omoplata merges. It writes a `reconciled/<queue>` head you can
`omo switch` onto; exit 2 means it carries conflict values (landable, resolve
later), and a strict queue refuses to keep them. Back to the local swarm:

```sh
for i in 1 2 3 4 5; do omo workspace add agent-$i ./agents/agent-$i --repo trunk; done
# each agent edits its own dir, then:
omo submit sub-$i --title "..." ws/agent-$i --repo trunk
omo land sub-$i --repo trunk          # concurrent lands are safe
```

When several submissions are ready together, **batch** them — omoplata checks
they are pairwise-disjoint *at definition granularity* relative to the queue's
landed state, validates them as one, and lands them in a single transaction:

```sh
omo land sub-a sub-b sub-c --repo trunk
```

Two agents who both edited `src/lib.rs` but touched **different definitions**
batch cleanly. Landing **auto-reconciles** (ADR-0010 Phase 3): the batch is
structurally merged against the queue's pre-land base and the `reconciled/<queue>`
merged trunk advances in the same transaction — so both agents' work survives
(not last-wins), *line-disjoint* edits to one definition merge, and an
*incompatible* same-definition pair rides through as a **conflict value** on a
permissive queue (`omo switch reconciled/trunk` to see the merged trunk). A
strict queue refuses to keep carried values — release lines stay clean. Use
`omo reconcile` explicitly only to preview a merge without landing.

Reconciliation is correct **across time**: each change merges against the trunk
it was *authored* on (recovered from the op log), not just the current head — so
if a teammate lands after you snapshot but before you land, their definitions are
preserved instead of looking reverted. You don't manage bases; it's automatic.

## Merging content: propose, then let the kernel check

`omo merge-file <base> <left> <right>` runs the Tier-2 driver (Rust `.rs` files
get the **structural** driver at definition granularity, recursing into
`impl`/`mod`/`trait` members; other files use Mergiraf if present, else a line
merge). The driver is an **untrusted proposer**: a clean proposal is passed
through the trusted kernel, which independently re-derives the merge and
**downgrades to a conflict** if it cannot witness the result. Read the exit code:

- **0** — clean *and* kernel-admitted. Use the output.
- **1** — an honest conflict (driver conflict, or kernel downgrade). The output
  has `<<<<<<<` markers; resolve them.
- **2** — clean merge, but it **carries** pre-existing conflict values that rode
  through untouched definitions (§5.4). It is landable; the values are resolved
  later. `stderr` says `N carried forward`.

Add `--validate '<cmd>'` to gate acceptance on a real check (P9): a clean,
kernel-admitted merge is materialized and `<cmd>` is run against it (`{}` = the
file path); a non-zero exit **demotes the merge to a semantic conflict** rather
than accepting something that does not build. This is how you stop a
structurally-clean-but-broken merge (e.g. two agents adding a same-named method)
before it lands.

`omo conflicts <file>` lists the conflict values a file carries, each pinned to
its definition. Exit 0 = none, 2 = some. Use it to find what still needs
resolving after conflicts rode through.

`omo rebase` / `omo autorebase` replay a change onto an advancing base and carry
conflicts forward as values instead of blocking.

## Release lines are queues, not branches

A release line is a **named landing queue with a policy object**, not a branch.
Policy lives in the repo; validation runs before the `Draft → Public` transition.

```sh
omo queue add release-1.2 --validate './regression.sh {}'   # strict by default: approval + no carried conflicts
omo queue list
omo land sub-42 --queue release-1.2       # gated on approval, carried-conflict rule, and the validator
```

- The implicit **`trunk`** queue is permissive (carried conflict values allowed —
  the fleet keeps landing); **registered queues default strict** (the release
  posture). A refused landing mutates nothing.
- **Backport = the same change landing in a second queue**, identity preserved,
  no cherry-pick: `omo backport sub-42 --to release-1.2`. It carries the approval
  forward under one of two **certificates**: *identity* when the content is
  byte-identical to what was reviewed, or *commutation* when the change moved
  since it landed (rebased past intervening landings) but every definition it
  changed matches the source queue's landed history — so nothing the reviewer
  approved was altered. A move that invents an unreviewed definition is refused
  pending re-review, naming the definition. After every land, `omo` prints the
  available backport commands for sibling queues.
- **"What still needs backporting" is a query, not a branch diff:**
  `omo revset 'landed(trunk) & ~landed(release-1.2)'`.

## Gotchas that bite git habits

- **Don't look for `commit` / `branch`.** There is no `omo commit` (workspaces
  auto-snapshot) and no `omo branch` (branches are deliberately not a primary
  object). Assemble state via workspaces, not pointers. **To take over a
  teammate's work and get the latest, use `omo switch <target>`** — it repoints a
  workspace at another change (`ws/<name>`, a change id, or a landed change) and
  materializes its live tip into the working dir. Since the repo is shared,
  that also brings in everything landed since; it refuses to clobber
  un-snapshotted edits unless `--force`.
- **Approve before landing into a strict queue,** or the land is refused (exit 2,
  stderr `not approved`).
- **Exit 2 is overloaded:** it means *either* "carried conflict values present"
  (a normal, landable state) *or* a hard error. Distinguish by reading `stderr`,
  not the code alone.
- **The structural merge is Rust-only.** Non-`.rs` files fall back to Mergiraf or
  line merge, so definition-granularity wins (clean disjoint merges, batch
  disjointness) apply to Rust.
- **Refs you'll see:** landed changes are `public/<change>` for trunk,
  `public/<queue>/<change>` for other queues; workspace tips are `ws/<name>`.
  Inspect with `omo ref list` and the history with `omo op log` (a true
  bi-temporal log — `omo op undo` is a real inverse, not a reflog pointer).

## When you're unsure

Reach for [`references/commands.md`](references/commands.md) for the exhaustive
command list, flags, and exit codes. The repo's `docs/user-guide.md` has
worked, executed examples of every workflow, and `docs/adr/0009-named-landing-queues.md`
explains the queue/policy design and its git-flow mapping.
