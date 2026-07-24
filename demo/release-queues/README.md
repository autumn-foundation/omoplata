# Demo: release lines as landing queues (ADR-0009)

The question from the swarm demo's SDLC discussion: git shops model release
lines as long-lived branches plus branch-keyed CI — what is the
omoplata-native shape? Answer: **a release line is a named landing queue with
a policy object**, not a branch. `./run.sh` walks the whole story with real
commands (needs `omo` built at `../../target/release/omo`).

## What the walkthrough shows

1. **Two landing targets, two postures.** The implicit `trunk` queue is
   permissive: no validator, carried conflict values allowed (§5.4 — the
   fleet keeps landing while a conflict awaits resolution). The registered
   `release-1.2` queue is strict: approval required, carried values refused,
   and a P9 validator (`regression.sh`) that runs the test suite against the
   submission's **materialized stored content** before anything transitions.

2. **Gate 1 — approval.** A `--pending` submission is refused by the release
   queue until `omo approve` records a reviewer.

3. **Gate 2 — P9 validation, in-band.** The approved hotfix lands only after
   the validator passes. Where branch-keyed CI runs *after* a merge commit
   exists (a failure breaks the branch until reverted), the queue's validator
   runs *before* the `Draft → Public` transition — a refused landing mutates
   nothing.

4. **Backports without cherry-picks.** The same change lands in `trunk` as a
   second landing of the *same identity*: one change object, two queues, two
   refs (`public/ws/hotfix` and `public/release-1.2/ws/hotfix`) pointing at
   the same tip. No identity fork, no patch-replay drift.

5. **A broken change never reaches the line.** `sub-208` fails the regression
   suite and is refused with a typed error; there is nothing to revert.

6. **Gate 3 — carried conflict values are policy-scoped.** `sub-209` carries
   an unresolved §5.4 conflict value (queryable with `omo conflicts`). Trunk
   lands it — noting the carriage — so fleet throughput never waits on
   resolution latency; the release line refuses it with an error naming the
   count. Same content, two policies, both honest.

## The git-flow mapping

| git flow | omoplata |
|---|---|
| `develop` branch | implicit `trunk` queue |
| `release/1.2` branch | `omo queue add release-1.2 …` |
| CI keyed on `release/*` | the queue's `--validate` command (P9, pre-landing) |
| branch protection / required review | `require_approval` policy |
| cherry-pick hotfix to release | `omo land <sub> --queue release-1.2` |
| "what's on 1.2?" | `public/release-1.2/*` refs, as-of-then via the op log |

Design rationale, alternatives, and future work (multi-approval thresholds,
queue verbs in revsets, batching, the landing daemon): see
[ADR-0009](../../docs/adr/0009-named-landing-queues.md).
