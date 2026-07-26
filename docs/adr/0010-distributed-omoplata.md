# ADR-0010: Distributed omoplata — remotes, replication, and remote landing authority

**Status:** accepted (Phases 1–2 implemented; Phase 3 implemented — `omo reconcile`
plus auto-wiring into `omo land`/`omo push`; cross-time base tracking remains).

## Context

Everything omoplata does today assumes **one shared `.omoplata` on one
filesystem**. ADR-0008 makes that safe for many concurrent processes (advisory
lock + atomic op-log writes), and the working model (§5.9) gives every agent its
own workspace over that one store. It is a genuinely good local story: N agents
land concurrently, disjoint definitions merge in the kernel, conflicts ride
through as values.

But "one filesystem" is a ceiling, not the design. The moment two developers on
two machines each want to run a swarm against a common substrate, "the refs are
already there because we share the op log" stops being true. Today the only way
across the gap is to **import a git repo, swarm locally, and export back** — a
batch round-trip through git's object format (ADR-0005) that loses omoplata's
op log, queue policy, and certificates at the boundary, and that no one wants to
run by hand between every sync.

This ADR decides how omoplata becomes distributed.

## The key insight: the trust boundary is *landing*, not transport

Git's distribution is transport-centric: replicate objects and refs, and refuse
(`non-fast-forward`) whenever two sides diverge, leaving the human to rebase.
omoplata does not have to inherit that, because its hard part is already solved
locally:

- **Merges are verified, not hopeful.** The kernel admits a merge only with a
  re-checkable witness (I8); anything it cannot witness degrades to a
  first-class conflict *value*, never a silent wrong answer.
- **Landing is a policy gate, not a pointer move** (ADR-0009): approval,
  carried-conflict rules, and P9 validation are checked in-band before
  `Draft → Public`.
- **Concurrent disjoint lands already merge in one transaction** — Tier-0 batch
  landing at definition granularity (ADR-0009) is exactly "accept several
  independent changes at once, refuse only a genuine same-definition
  collision."
- **Approval can travel under a certificate** (ADR-0009, I5): identity and
  commutation certificates let a change's review carry to new content a remote
  can re-check without trusting the sender.

So for omoplata the scarce, security-relevant resource is **who is allowed to
land, under what policy** — not who can move a ref. That reframes distribution:
replicate the content-addressed store freely, but make **landing an authority**.

## Decision

Distribution is **replication of the content-addressed object store + selective
ref sync, with a remote acting as the landing authority** — the natural home of
ADR-0008's Option C single-writer landing daemon, now across machines.

### Ref model

- `ws/<name>` refs are **private and local** — a workspace's in-progress tip.
  They are never fetched; to share in-progress work you land it (or, Phase 2,
  push a submission).
- `public/<change>` and `public/<queue>/<change>` are the **shared truth** — the
  landed, policy-passed state.
- A fetch writes the remote's public refs into a **remote-tracking namespace**,
  `remotes/<name>/<ref>` (mirroring git's `refs/remotes/origin/*`). Fetch never
  silently merges remote landings into your local `public/*`; adopting them is a
  separate, explicit act (`omo switch` to a remote-tracking ref today; a local
  land/merge later).

### Op-log reconciliation

The landing authority **serializes landing ops** (Option C: a single writer owns
the queues). Clients therefore only ever *fast-forward* their view of the
authority's `public/*`; they never need a global order over their own local ops
(workspace snapshots), which stay local until submitted. Full multi-master
op-log reconciliation (two authorities that independently landed, merged via a
CRDT-like op-log join) is **explicitly out of scope** — the kernel could make it
sound, but a single landing authority is simpler and sufficient for "many devs,
one substrate."

### Transport

Start with the **local-path transport**: a remote is another `.omoplata`
reachable as a filesystem path (a shared mount, an NFS export, a sibling clone).
Replication is a copy of the content-addressed object closure — trivially
idempotent, since equal content has equal id. Networked transports (http/ssh)
and a git-compatible `push`/`receive-pack` path are future work, mirroring how
git interop deliberately began at `file://` before the wire (ADR-0005). A
git-compatible remote stays valuable for interop but is *lossy at the boundary*
(git carries commits/trees/refs, not op-log entries, queue policy, or
certificates), so it can never be the full-fidelity substrate — only the native
remote preserves the whole model.

## Phasing

- **Phase 1 — read-replication (this ADR ships it).** `omo remote add/list/remove`
  registers named remotes (paths). `omo fetch <remote>` copies the object
  closure of the remote's `public/*` refs into the local store and records them
  under `remotes/<name>/*` via the op log. `omo switch` resolves remote-tracking
  refs (`remotes/origin/…`, or the `origin/<change>` shorthand), so a developer
  can **switch straight onto a teammate's remote-landed work** — the same
  one-liner as local switch, now across machines. Replication only: no remote
  landing yet.
- **Phase 2 — remote submit/land (this ADR ships it).** `omo push <remote> <id>`
  replicates a submission's change content into the remote store, records the
  submission (so its approval and any certificates travel), and runs the
  *remote's* queue policy against the *remote's* landed state — the
  carried-conflict rule, P9 validation, and definition-granular batch
  disjointness are re-checked there, and the landing happens under the remote's
  lock. A refused landing mutates nothing. The client cannot bypass the gates;
  it can only propose content the remote then re-validates against its own
  trunk. **Known limitation:** approval *authenticity* is still trusted — the
  remote honours the submission's recorded approval rather than re-deriving who
  approved it. A signed/attested approval (so the authority verifies the
  reviewer, not just the assertion) is the natural next hardening; the ADR-0009
  certificate machinery is the primitive it builds on.
- **Phase 3 — optimistic concurrent landing with kernel reconciliation
  (primitive shipped).** Many swarms push concurrently; the authority merges
  disjoint-definition work in a single transaction and carries genuine conflicts
  forward as values, refusing only against a queue that forbids carried values —
  *as values, not as push rejections*. This is where omoplata decisively beats
  git's non-fast-forward model.

  The reconciliation primitive is `omo reconcile <id…>`: it folds several
  submissions through the Tier-2 structural driver against the queue's current
  landed state (their **shared base** — the real common ancestor of
  *simultaneous* work), so edits to different definitions of one file combine
  into a single tree and edits to the *same* definition surface as first-class
  conflict values (§5.4). The result is written to a `reconciled/<queue>` head
  you can `omo switch` onto; a strict queue still refuses to keep carried values.
  This is the exact case a shared-base overlay could not represent before: two
  concurrent changes to one file now merge into one tree instead of one silently
  winning.

  **Auto-wired.** `omo land` and `omo push` reconcile automatically: each
  computes the merge against the queue's *pre-land* base (its true shared
  ancestor, since the reconciliation runs before the landing writes anything) and
  advances the `reconciled/<queue>` head in the *same* locked transaction as the
  landing. No explicit `omo reconcile` step is needed — the authority always
  presents a merged trunk. This also **sharpens batch landing** (the ADR-0009
  future-work item): landing decides conflicts *structurally* rather than by the
  coarse definition-support check, so two line-disjoint edits to one definition
  now merge and land instead of refusing, while an incompatible pair rides
  through as a conflict value on a permissive queue (a strict queue still refuses
  to keep carried values, keeping release lines clean). `omo reconcile` remains
  as the explicit primitive for reconciling without landing.

  **Still open on this phase — cross-time base tracking.** The shared-base fold
  is exactly right when the inputs were made against the current landed state
  (the simultaneous case). A change made against an **older** head has no recorded
  base (the store has no commit parents — ADR-0002), so reconciling it against the
  current head over-approximates and conservatively surfaces more conflict values
  than a true three-way against its real base would. A per-change base pointer (or
  an op-log-derived base) is the missing piece, and the natural next step.

## Consequences

- Phase 1 makes `omo switch` (§5.9) genuinely distributed with no new network
  code and no git round-trip, and establishes the transport + remote-tracking
  seams the later phases extend.
- Fetch pulls **landed public state**, not private in-progress workspaces —
  honest about what is shareable without a landing decision.
- The heavy lifting (verified merge, conflicts-as-values, certificates, batch
  landing) already exists; distribution *reuses* it rather than re-deriving
  trust at the transport layer. The remaining work is transport reach (network)
  and moving the landing authority off-box, not new correctness machinery.

## Non-goals (for now)

- Networked (http/ssh) transports; `git push` / `receive-pack` encoding.
- Multi-master op-log reconciliation between independent authorities.
- Attested approval — the remote trusts the submission's recorded approval
  (Phase 2 ships the landing gate; verifying *who* approved is later hardening).
- Cross-time base tracking: reconciling a change made against an older head using
  its true base rather than the current head (needs a per-change base pointer;
  the store has no commit parents today). `omo reconcile` and the auto-wired
  `land`/`push` merge against the current landed state — exact for simultaneous
  work, conservative (extra conflict values) for a change built on an older head.
