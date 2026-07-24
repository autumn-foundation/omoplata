# ADR-0009: named landing queues with per-queue policy — release lines without release branches

- Status: Accepted
- Date: 2026-07-24

## Context

The design doc's landing model (§5.10) is a single merge queue: `land` gates on
approvals plus dynamic validation (P9) and performs the `Draft → Public` phase
transition (P5). That is the right spine, but real software lifecycles carry
more than one landing target: a trunk that moves fast, one or more **release
lines** that move deliberately, hotfix backports, and per-target CI. In a git
shop those are modeled as long-lived branches (`develop`, `release/1.2`) plus
forge-side CI rules keyed on branch names — machinery omoplata deliberately
does not have, because branches are not primary objects (§5.9).

The question this ADR answers: **what is the omoplata-native shape of a
release line, and where does per-target CI attach?**

Two observations drive the design:

1. A release branch is two things fused together: a *pointer* (what is on the
   line) and a *policy* (what may land there, gated by which checks and which
   approvals). The pointer omoplata already has — refs folded from the op log.
   The policy has no home; it is implicit in forge configuration, outside the
   VCS, keyed on branch *names*.
2. Branch-keyed CI runs **after** a merge commit exists; a failure leaves the
   branch broken until someone reverts. The design doc's P9 posture is the
   opposite: *acceptance is provisional pending dynamic validation* — the check
   belongs **inside** the landing gate, before the phase transition.

The swarm demo (`demo/swarm-vs-gitflow`) sharpened one more requirement:
§5.4's conflicts-as-values means trunk may legitimately carry unresolved
conflict values while the fleet keeps landing around them (round 3 of the
demo). Whether that is acceptable is *itself* a per-target policy: right for a
fleet trunk, wrong for a release line.

## Decision

**A landing queue is a named, persisted policy object; `land` targets a queue.**

```text
omo queue add release-1.2 --validate './regression.sh {}'
omo land sub-42 --queue release-1.2
```

- **`QueuePolicy`** (in `omoplata-work::queue`) carries: `name`,
  `description`, `validate` (a P9 validator command run against the
  submission's **materialized content** before landing; `{}` substitutes the
  content directory), `require_approval`, and `allow_carried` (whether content
  still carrying §5.4 conflict values may land).
- **`QueueRegistry`** persists policies at `.omoplata/queues.json` with the
  same discipline as the workspace registry: pretty JSON, crash-atomic
  `atomic_write`, mutation only under the repository `flock` (ADR-0008).
- **The `trunk` queue exists implicitly** with the permissive fleet posture:
  approval required, no validator, carried values **allowed** — landing
  throughput must not wait on resolution latency (§5.4, demo round 3).
  Registering a queue named `trunk` overrides the implicit policy; registered
  queues default to the strict posture (carried values refused), which is what
  a release line wants.
- **Gate evaluation is split observer/judge**, the same shape as
  driver-proposes/kernel-admits: the CLI *observes* the facts — materializes
  the submission's stored trees into a scratch directory, counts conflict
  values with the same scanner the structural driver uses, runs the validator —
  and `land_submission_in_queue` *applies policy* to the observed
  `QueueGates`, refusing with a typed error (`QueueCarriedRefused`,
  `QueueValidationFailed`, `SubmissionNotApproved`) before any state changes.
  A refused landing mutates nothing.
- **Per-queue refs.** Landing into `trunk` writes the legacy
  `public/<change>` refs; every other queue writes
  `public/<queue>/<change>`. The *same change* can therefore land in several
  queues — the backport story with identity preserved: one change object, two
  landings, two refs, no cherry-pick fork. The op-log entry notes the queue.

### The SDLC mapping, explicitly

| git-flow concept | omoplata shape |
|---|---|
| `develop` / trunk branch | the implicit `trunk` queue |
| `release/1.2` branch | `omo queue add release-1.2 …` + its `public/release-1.2/*` refs |
| branch-filtered CI (`on: push: branches: [release/*]`) | the queue's `validate` command, run pre-landing (P9) |
| branch protection / required reviews | `require_approval` (thresholds: future work, below) |
| cherry-pick a hotfix to the release branch | `omo land <sub> --queue release-1.2` — same change, second landing |
| "what is on release 1.2?" | the `public/release-1.2/*` refs; as-of-then views via the bi-temporal op log (§5.6) |

## Options considered

### Option A — policy objects on the landing gate ✅ (this ADR)

As above. Policy lives in the repository, versioned next to the data it
governs; validation is in-band and pre-transition; queues are cheap (a JSON
entry, not a ref-namespace ceremony).

### Option B — model release lines as bookmarks/branches

Reintroduce long-lived mutable branch pointers and key validation on their
names. Rejected: §5.9 is explicit that local mutable branch pointers are the
"pointer religion the change graph exists to escape", and it would leave
policy outside the VCS again (the forge-CI shape, with its after-the-fact
failures).

### Option C — a single queue with per-submission flags

Pass `--validate`/`--strict` at each `land`. Rejected: policy belongs to the
*repo*, not the invocation (§5.10's approval-carry-forward discussion makes
the same call); per-invocation flags are unauditable and trivially forgotten
by an agent fleet.

## Consequences

- A release line costs one command to create and is queryable like everything
  else; there is no branch to keep merged, only a second landing target.
- Validation failures cannot break a line: the refused submission simply never
  transitions, and the op log never records a landing. This is the P9 posture
  extended from merges to landings.
- The carried-values rule makes §5.4's permissiveness *scoped*: trunk keeps
  landing around unresolved conflicts (demo round 3), while `--queue
  release-1.2` refuses them with an honest, typed error naming the count.
- Landed refs are per-queue, so "what does 1.2 contain that trunk doesn't"
  is a ref-set difference today and a revset (`landed(release-1.2) &
  ~landed(trunk)`, §5.8) once queue verbs land in the revset language.

## Future work

Four of the items originally listed here have since shipped in their v1
shapes: **`landed(<queue>)` in revsets** (registry-driven ref disambiguation;
`landed(trunk) & ~landed(release-1.2)` is the needs-backport query),
**Tier-0 batching** (`omo land a b c` — batch validated as one, landed in one
locked transaction), **definition-granularity Tier-0 disjointness** (support is
the set of definitions a submission changed *relative to the queue's landed
base*, computed by `rust_support`; two agents editing different definitions of
one file batch, while a shared definition refuses the batch naming it —
containers compare by their shell so member additions stay disjoint), and
**mechanical backport offers** (`omo backport` carries approval forward with an
*identity* certificate — content byte-identical to the reviewed, landed tip;
moved content demands re-review).

Still open:

- **Multi-approval thresholds and named-reviewer policies.** `Approval` is a
  single-reviewer assertion today; §5.6 wants approvals as bi-temporal,
  revocable assertions. When that model lands, `require_approval: bool`
  generalizes to a count or reviewer-set predicate without changing the gate's
  shape.
- **Commutation-certificate backports for moved content** (I5): today's
  backport certificate covers only the identity case; a checked-commutation
  certificate would carry approval across content that provably rebased
  cleanly. The same algebra would sharpen batch disjointness from "changed the
  same definition" to "changed the same definition *incompatibly*", letting
  line-disjoint edits to one definition batch too.
- **`queued()`** awaits persistent queue membership (landing is immediate in
  v1), which arrives with the single-writer landing daemon (ADR-0008 Option C)
  that owns all queues.
