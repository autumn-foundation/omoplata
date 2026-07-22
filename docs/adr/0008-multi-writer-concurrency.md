# ADR-0008: multi-writer safety for `.omoplata` — advisory locking + atomic op-log writes, with a single-writer daemon as the documented future

- Status: Accepted
- Date: 2026-07-22

## Context

omoplata's **object store** is already safe under concurrency (ADR-0002): objects
are content-addressed, written to a temp file and `rename`d into place, and
idempotent — two processes writing the same object converge on the same bytes at
the same address, and a reader never observes a partial object. Nothing about
the object store needs to change for multiple `omo` processes to share a repo.

The **mutable** state is not safe. Two surfaces in `.omoplata/` are mutated by a
read-modify-write cycle with no mutual exclusion and no crash-atomic write:

1. **The refs**, which are *not* stored as files — they are folded out of the
   bi-temporal operation log (`OpLog::refs_now` / `refs_at`, §5.6, I7).
2. **The operation log itself**, `.omoplata/oplog.jsonl`, which every mutation
   appends to.

Every writer — `omo ref set`, `omo op undo`, `omo autorebase` — performs the
same unguarded cycle:

```text
log = OpLog::load(".omoplata/oplog.jsonl")   // read whole file
log.mutate(...)                              // compute old->new in memory, append
log.save(".omoplata/oplog.jsonl")            // rewrite whole file
```

Today `OpLog::save` is a single `std::fs::write` of the whole serialized log.
Under two or more concurrent `omo` processes this loses data and can corrupt the
file. Three distinct races follow.

### Race 1 — ref-update read-modify-write (lost update)

`omo ref set main <c>` loads the log, calls `OpLog::set_ref`, which reads the
*current* target of `main` as `old` (so the op is invertible, I7), appends a
`SetRef { old, new }`, and saves. `omo op undo` and `omo autorebase` are the same
shape: **load → compute against the state just read → append → save the whole
file**.

Interleave two writers A and B, each starting from a log of length *N*:

```text
A: load  (N ops)          B: load  (N ops)
A: append -> N+1 ops
A: save  (writes N+1)
                          B: append -> N+1 ops   (B never saw A's op)
                          B: save  (writes N+1)   (clobbers A entirely)
```

B's whole-file rewrite is computed from the pre-A snapshot, so **A's operation is
silently lost**, the file length is *N+1* not *N+2*, and B's `SetRef.old` was
captured against a ref state that no longer existed — its inverse is now wrong,
breaking I7. No error is raised: a writer that "succeeded" has vanished.

### Race 2 — op-log append interleaving / torn file (corruption)

`save` rewrites the entire file. Two overlapping `save`s, or a crash (SIGKILL,
power loss, disk-full) part-way through a `save`, leave the file in an
intermediate state: a truncated final JSON line, or bytes of one writer's log
overwritten by a shorter log from another. On the next `OpLog::load`, the partial
line fails `serde_json::from_str` and the **entire repository fails to load** —
not just the last op. A whole-file, non-atomic rewrite has no safe intermediate
state.

### Race 3 — `refs_at` / as-of-then read of a torn file (inconsistent read)

Refs and the bi-temporal `refs_at(seq)` / `tip_as_of` queries are derived by
folding the log. A reader (`omo ref list`, `omo revset`, `omo op log`) that reads
the file *while a writer is mid-`save`* can observe a half-written file: a
prefix of the new content, or new content truncated to the old length. The
transaction-time guarantee of §5.6 — "the as-of-*t* view is stable forever" — is
violated the moment a reader can see a state that never atomically existed. The
bi-temporal query must never observe a partial append.

## Options considered

### Option A — advisory file locking + atomic writes  ✅ (v1)

Two independent, composable mechanisms:

- **Mutual exclusion via `flock(2)`** on a dedicated `.omoplata/lock` file. A
  writer takes an **exclusive** advisory lock for the whole `load → mutate →
  save` critical section; the OS serializes writers. The decisive property:
  **`flock` is released automatically when the holding file descriptor is
  closed, including on process exit and on a crash** — the kernel drops the lock
  when the process dies. There is no lock record to leave behind and therefore
  no stale-lock problem.

- **Crash-atomic writes** for `oplog.jsonl` (and, for free, objects):
  **write to a temp file → `fsync` the temp file → `rename` over the target →
  `fsync` the parent directory.** `rename(2)` is atomic within a filesystem, so a
  reader sees either the complete old file or the complete new file, never a
  torn one — closing Race 2 and Race 3 *even for lock-free readers*. The two
  `fsync`s make the guarantee survive a crash: the data is durable before the
  rename is exposed, and the rename itself is durable after the directory sync.

### Option B — `O_EXCL` PID lockfile

Create `.omoplata/lock` with `O_CREAT | O_EXCL`, write the owning PID, delete on
release. `O_EXCL` gives mutual exclusion, but the lock is **not** tied to process
liveness: if the holder is `kill -9`ed or the machine loses power mid-critical-
section, the lockfile remains and **every future writer is wedged** until a human
removes it. Recovering requires *stale-lock detection* — read the PID, check
whether it is alive (racy: PIDs are reused), possibly a heartbeat/timeout — none
of which is crash-safe without more machinery. `flock` gets liveness-tied release
from the kernel for free, so Option B is strictly more code for a weaker
guarantee.

### Option C — single-writer landing daemon

Run one long-lived process that **owns** the op log; all mutations are submitted
to it over a socket/queue and it applies them serially. This is the *strongest*
design: there is exactly one writer, so the read-modify-write cycle is trivially
race-free without any file lock, and the daemon can batch, validate, and order
landings globally. The design doc's landing queue "conveniently already wants to
be" exactly this component.

The cost is a process-lifecycle, IPC, and crash-recovery surface that is out of
proportion to v1's need (make the CLI safe for a handful of concurrent
invocations against a local repo). It is the right *destination*, not the right
first step.

## Decision

**v1 ships Option A: advisory `flock` locking plus atomic op-log writes.** The
single-writer daemon (Option C) is the **documented future**, and the v1 locking
API is shaped so the daemon is a drop-in evolution rather than a rewrite.

Concretely:

- **`omoplata-store`** gains a `RepoLock` RAII guard and
  `Repository::lock()` / `Repository::try_lock()`. `lock()` opens/creates
  `.omoplata/lock` and takes an exclusive `flock`; the lock is released when the
  returned guard is dropped, and — inherently — when the process dies. A shared
  `atomic_write(path, bytes)` helper (temp → fsync → rename → dir fsync) backs
  both the op-log save and object writes.

- **`omoplata-work`** makes `OpLog::save` use `atomic_write`, so the log file is
  never torn and a crash cannot corrupt it. `OpLog::mutate_locked(repo, |log| …)`
  performs the whole locked read-modify-append: it takes `repo.lock()`, loads,
  runs the caller's mutation, and saves atomically — the entire cycle under one
  lock. `OpLog::load` tolerates a missing file (fresh repo) and, thanks to atomic
  rename, never observes a partial file.

- **`omoplata-cli`** wraps every op-log-*mutating* command (`ref set`,
  `op undo`, `autorebase`) so it holds `repo.lock()` across the whole
  `load → mutate → save`. Read-only commands (`ref list`, `op log`, `revset`)
  read **without** the lock: atomic rename guarantees they always see a complete
  file, so blocking them behind the writer lock would add contention for no
  correctness benefit.

### Why `flock` over a PID lockfile

Crash-safety. `flock` is released by the kernel when the owning fd closes, which
happens on normal exit **and** on `kill -9`/panic/power-loss-then-reboot. A PID
lockfile survives the crash and wedges the repo until manual cleanup, and
detecting that it is stale is racy (PID reuse) and never fully crash-safe. `flock`
gives liveness-tied release for free, so there is **no stale-lock bookkeeping**.

### The crash-safety guarantee, stated explicitly

`OpLog::save` (via `atomic_write`) guarantees that at every instant the on-disk
`oplog.jsonl` is **either the complete pre-save log or the complete post-save
log**, never a mixture and never truncated, and that after `save` returns the new
content is durable. This holds because:

1. the new content is written to a temp file and `fsync`ed **before** it is
   linked to the real name (durable data);
2. `rename(2)` atomically swaps the name to the fully-written temp file (no torn
   reader ever sees a prefix);
3. the parent directory is `fsync`ed **after** the rename (the rename itself
   survives a crash).

A crash at any point leaves the old complete file (rename not yet done) or the
new complete file (rename done) — the log can never be corrupted.

### The mutual-exclusion guarantee

While a `RepoLock` guard is alive, no other process can hold the exclusive lock on
the same `.omoplata/lock`; `lock()` blocks until it can. Wrapping
`load → mutate → save` in the guard makes the read-modify-write cycle atomic
*across processes*, closing Race 1: the second writer cannot load until the first
has saved and dropped the lock, so it always folds its `old` against the first
writer's committed state, and no update is lost.

## How the daemon reuses this layer (the documented future)

The daemon (Option C) is the natural next step, and the v1 API anticipates it:

- The `RepoLock` guard becomes the **daemon's internal invariant** — the daemon
  holds the repository lock for its lifetime (or simply *is* the only writer), so
  "hold the lock across load→mutate→save" becomes "the single writer owns the
  log," the same critical section without per-command lock acquisition.
- The CLI's writer path (`OpLog::mutate_locked`) becomes "**submit the mutation
  to the daemon**" — the daemon runs the identical load→mutate→save under its own
  ownership. The mutation closures are already the unit of work a landing queue
  would enqueue.
- `atomic_write` and the append-only log are unchanged: the daemon still wants
  crash-atomic persistence.

So v1 locking is not throwaway scaffolding — it is the mechanism the daemon
internalizes.

## Consequences

- Multiple `omo` processes can safely share one `.omoplata/`: writers serialize
  on `flock`, no update is lost, the log is never torn, and readers always see a
  complete file. Proven by a **12-process concurrency stress test**
  (`crates/omoplata-cli/tests/concurrency.rs`) that spawns 12 real OS processes
  hammering the same repo and asserts, after they join: the log parses fully (no
  torn JSON), exactly `12 × K` operations are present (no lost update), the `seq`
  set is exactly `0..12·K` (monotonic, gap-free, no duplicates), and every
  writer's final ref survives.
- Single-process behavior, flags, and output are byte-for-byte unchanged;
  locking and the extra `fsync`s are the only additions.
- **Cross-process only.** `flock` serializes *processes*; it does not guard two
  threads of one process sharing a `Repository` — v1's model is one repo mutation
  per `omo` invocation, which is exactly what the CLI does.
- **Future work.** The single-writer landing daemon (Option C) that internalizes
  the lock; lock-acquisition timeouts / `try_lock`-based "repo busy" UX for
  scripts; and per-ref or op-range locking if global serialization ever becomes a
  throughput bottleneck (it is not at CLI scale).
```
