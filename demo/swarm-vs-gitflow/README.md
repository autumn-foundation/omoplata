# Demo: agent-swarm development — omoplata workflow vs git flow

**Question:** for a swarm of coding agents working the same codebase concurrently,
is the omoplata workflow (per-agent workspaces → auto-snapshotted change stacks →
`submit` → `land` through the merge queue, content integrated by the Tier-2
structural merge under kernel admission) actually better than git flow (feature
branch per agent off `develop`, line-based `git merge`)? Is it worth pursuing?

**Method:** five real LLM agents were given independent tasks on copies of the same
small Rust crate (`base/`), engineered to collide: two agents appending different
features at the same file position, edits to adjacent definitions, and two agents
modifying the *same function* (a genuine semantic conflict). The identical
edit-sets (captured in `patches/`) were then integrated through both workflows in
the same land order — the order the omoplata merge queue actually produced under
five concurrent `submit`+`land` processes. Two targeted stress cases and a
10-writer contention test followed. `./run.sh` reproduces everything without LLM
agents (needs `omo` built at `../../target/release/omo`).

## Results

### Round 1 — the live swarm (5 agents, same edits through both systems)

| | omoplata | git flow |
|---|---|---|
| Auto-clean integrations | 3 | 4 |
| Genuine conflict (same fn edited twice) | 1, honest, scoped to the definition's interior; rest of file merged | 1, equivalent presentation |
| Kernel downgrades | 1 (structural proposal was *correct*; kernel could not witness it at line level → demanded validation) | n/a — no such gate exists |
| Concurrent landings, one shared repo | 5/5 succeeded; op log seq `0..9` gap-free (flock + atomic writes, ADR-0008) | commits/merges serialized by hand; see contention test |
| Final trunk | compiles, tests pass | compiles, tests pass |

Honest surprise: the predicted "both agents append at EOF" false conflict did
**not** fire in git — its line merge anchored the two hunks apart and got it
right. Round 1 alone is close to a wash on merge quality: git conflicted once
(the genuine conflict), omoplata conflicted once genuinely plus one downgrade
that cost a validation step without being a real conflict.

Footnote from the live run: the operator's *first manual resolution of the git
conflict was botched* (duplicated trailing lines) and only caught by `cargo
test` afterwards. Not a git defect — but a reminder of how error-prone manual
marker surgery is, which is precisely the step both `omo`'s scoped
conflicts-as-values and P9 validation shrink.

### Round 2 — the silent-wrong-answer probes

**R2a, move + edit** (one agent moves `priority_of` to the bottom of the file;
another edits its body in place). Git conflicts at the old site *and* silently
plants the **stale** copy at the new site: resolving the visible conflict as
"accept the move" — the natural reading — loses the edit with no warning.
omoplata's structural driver matched the definition through the move and
produced **one copy with the edit applied** (kernel downgraded pending
validation, and validation passes).

**R2b, duplicate work** (two agents independently implement `is_empty`, different
bodies, different positions in the impl block). Git exits 0 with two copies of
the method — a file that does not compile — and has nothing below forge-side CI
to catch it. In the *first* run of this demo, omoplata failed the same way (the
Tier-2 driver line-merged impl interiors); that finding drove the
member-granularity extension below, and the driver now surfaces an **honest,
member-scoped conflict** (both variants inside one marker block, exit 1).
Identical double-adds dedupe to a single copy. P9 `--validate` remains the
backstop for whatever granularity misses.

### Round 3 — conflicts as values: the queue that never blocks

With conflict-value propagation in the driver (§5.4, P3), the round-1 sequence
replays **without resolving the genuine conflict first**:

1. agents 3, 1, 2 land clean;
2. agent-5's landing produces the genuine conflict — the conflicted output is
   adopted as trunk, *unresolved*;
3. agent-4 lands **on top of the conflicted trunk**: exit 2,
   `0 new conflict(s), 1 carried forward` — his feature integrates
   structurally around the conflict, which rides through byte-identically;
4. `omo conflicts trunk.rs` pins the value: `priority_of line 16`;
5. the resolution is applied **last** ("resolution is a commit that collapses
   the term"); final tests pass.

Before this change, a conflict in trunk poisoned the parse gate and degraded
every subsequent structural merge to a line merge — the queue had to stop for
a human. Now landing throughput is independent of resolution latency: exactly
the property a 20–30-agent swarm needs, where one stuck conflict must not
stall the other 29 agents' landings.

### Contention — 10 concurrent writers, one shared repo

| | survived |
|---|---|
| git (10 parallel `add`+`commit`) | **1–2 / 10** — the rest die on `index.lock` |
| omoplata (10 parallel `workspace add`+`submit`+`land`) | **10 / 10** — op log complete and gap-free |

Git's answer is a worktree or clone per agent plus push/rebase retry loops
against the shared remote — real coordination machinery the orchestrator must
build and babysit. omoplata's answer is the repo itself: workspaces are
first-class, and every mutation is flock-serialized and crash-atomic.

## Verdict

**Worth pursuing — the advantage is real but it is not where the pitch usually
points.** On plain merge quality over well-partitioned agent tasks, git line
merge held up better than expected (round 1 was nearly a tie). The compounding
advantages for swarm development are:

1. **Multi-writer mechanics.** N agents against one repo with zero orchestration
   scaffolding, versus git needing per-agent clones/worktrees and retry loops.
   This gap widens with fleet size.
2. **Refactor tolerance.** Move+edit — constant in agent workloads, where one
   agent reorganizes while another patches — is a silent-data-loss trap in git
   and merged correctly by definition identity here (R2a).
3. **An in-band honesty gate.** Kernel admission + P9 validation put "prove it
   or conflict" inside the landing path. It caught omoplata's own driver
   producing a broken merge (R2b) — the system distrusts its own proposer,
   which is exactly the property a swarm operator wants when no human reads
   every diff.

4. **Non-blocking landing under conflict** (round 3). Conflicts ride through
   subsequent landings as first-class, queryable values and are resolved when
   convenient — landing throughput decouples from resolution latency, which is
   what lets a large fleet keep landing while one conflict waits for a human.

Costs observed: kernel downgrades tax legitimate merges with validation steps
(1 of 5 landings in round 1). The two gaps the first run of this demo exposed —
line-merged impl interiors (R2b) and the parse gate rejecting conflict-carrying
files — were closed by the member-granularity and conflict-value-propagation
extensions to the Rust structural driver; R2b and round 3 above measure the
result.

## Files

- `base/` — the shared crate the swarm edited
- `patches/agent-{1..5}.patch` — the five agents' actual edit-sets
- `run.sh` — reproduces both tracks, round 2, and the contention test; prints a summary table
- `out/` — generated by `run.sh` (not committed)
