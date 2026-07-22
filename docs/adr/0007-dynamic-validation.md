# ADR-0007: kernel admission is provisional — dynamic validation (P9) demotes a failing merge to a Tier-3 semantic conflict

- Status: Accepted
- Date: 2026-07-22

## Context
The design doc's principle **P9 — Dynamic validation over static omniscience**
(§3) rejects trying to prove *behavioral* merge correctness statically:

> Behavioral merge correctness is undecidable and per-language static analysis is
> a tar pit. Instead: every kernel-accepted merge above Tier 1 is *provisional*
> until the merge commit passes build + test in CI. A failed validation demotes
> the merge to a semantic conflict carrying both sides' intent metadata. This
> trades a research problem for infrastructure the agent fleet (Sentinel) already
> operates.

§4 makes this a property of the Tier-2 admission rule itself: after tree-equality
and trivia conservation (I11) checks, **"Acceptance is provisional pending
dynamic validation (P9)."** And §4's Tier-3 definition names the demotion target:
**"What survives Tiers 1–2, *or fails dynamic validation*, is presented as a
semantic conflict: both sides' definition-level intent, provenance … — not
`<<<<<<<` soup."**

The kernel's LCF admission boundary (ADR-0002, invariant **I8**) already
guarantees *structural* soundness — a merge the kernel emits carries a checked
commutation witness, or it is a `Conflict` value; there is no third output. But
I8 says nothing about whether the merged tree *builds and passes tests*. Two
edits can commute at the line/tree level and still combine into something that
does not compile or fails its suite. P9 is the answer: that structural admission
is only **provisional**.

The obstacle is that the doc's validator is *CI running build + test* — and
running a real per-language build + test loop against an arbitrary merged tree is
environment- and toolchain-specific. What is tractable, and what the doc actually
specifies, is the **policy**: given a validation verdict, a failure must demote,
never silently accept.

## Decision
Encode P9 as a **pure demotion policy** over the kernel's admission, plus a thin
CLI wiring that runs a real validator, plus a CI job that makes the repository's
own CI the concrete dynamic validator.

- **`omoplata_algebra::validation` — the pure policy.** `dynamic_validate(base,
  left, right, admission, passed, reason) -> Validated` maps a kernel
  `Admission` and a validation verdict to one of two terminal states:
  - `Merged` + `passed` ⇒ `Validated::Accepted(result)` — the provisional merge
    stands;
  - `Merged` + `!passed` ⇒ `Validated::Demoted { conflict, reason }` — a **real
    Tier-3 `Conflict` value** carrying `base` and *both sides'* full content as
    its sides, plus the failure reason. A failed validation does **not** yield a
    silently-accepted wrong merge; it becomes a conflict value, consistent with
    I8 ("no third output") and I12 ("degrade to a fresh conflict rather than
    silently selecting an outcome").
  - `Conflict(..)` ⇒ `Validated::Demoted` carrying that conflict unchanged —
    there is nothing provisional to validate.

  The function is pure, total, and I/O-free: it is the P9 *policy*, testable in
  isolation. Marked `// PROOF OBLIGATION (P9/I12)` at its definition.

- **`omo merge-file --validate <cmd>` — running a real validator.** When the
  driver's proposal is clean *and* the kernel admits it, that admission is
  treated as provisional: the merged output is materialized to a temp file and
  `<cmd>` is run as a shell command. **Convention:** a `{}` placeholder in
  `<cmd>` is substituted with the temp file path; if there is no placeholder, the
  path is appended as the last argument. The validator's exit status becomes the
  `passed` boolean fed through `dynamic_validate`. On pass: `dynamic validation
  PASSED` (stderr), merged doc (stdout), exit 0. On fail: `dynamic validation
  FAILED: demoted to semantic conflict (<reason>)` (stderr), the Tier-3
  semantic-conflict view (stdout), exit non-zero. The validator is **never** run
  when the merge already conflicted (driver conflict or kernel downgrade) —
  nothing provisional to validate. Without `--validate`, behavior is byte-for-byte
  the previous behavior.

- **The repository's own CI is the dynamic validator.** `.github/workflows/ci.yml`
  gains a `dynamic-validation` job (after `build`) that builds `omo` and proves
  both P9 paths on a clean, kernel-admitted merge: `--validate 'true'` accepts
  (exit 0), and `--validate 'false'` demotes to a semantic conflict (non-zero,
  swallowed by an `if` so the job stays green while asserting the demotion). This
  is the doc's "passes build + test in CI" made concrete at the scale this
  environment supports.

## Relationship to I12
**I12 (runtime confluence check / resolution admission)** is the per-instance
runtime guard this realizes. I12 says that at resolution time a check that could
only fail through a kernel bug, if it fails, "degrades to a fresh conflict rather
than silently selecting an outcome." P9's dynamic validation is the same
discipline one step earlier in the lifecycle: a merge the kernel *provisionally*
admitted, but that the configured validator (build + test) rejects, **degrades to
a fresh Tier-3 conflict** rather than standing as an accepted merge that does not
build or pass tests. `dynamic_validate` is where that per-instance guard lives
for the validation step; the repo's CI job is the validator that trips it.

## The reduction, stated plainly
**The validator is a configured command, not a per-language build+test loop the
tool drives itself.** `omo merge-file --validate` runs whatever shell command it
is given and trusts its exit status; it does not know how to build or test an
arbitrary tree. In production the command *is* "run CI" (or a Sentinel job); in
this repository the concrete validator is the `dynamic-validation` CI job, and in
tests it is `true` / `false` so the outcome is deterministic and toolchain-free.
What is fully real and load-bearing is the **policy**: a clean, kernel-admitted
merge is provisional, and a failing verdict demotes it to a first-class Tier-3
conflict value — never a silently-accepted wrong merge.

## Consequences
- P9 is demonstrable today: a clean merge with a passing validator is accepted,
  and the same merge with a failing validator is demoted to a semantic conflict
  (exit non-zero) — exercised by unit tests on `dynamic_validate`, CLI tests on
  `omo merge-file --validate`, and the CI `dynamic-validation` job.
- The demotion carries both sides' intent (base + left + right), so a downstream
  resolver (human or arbiter agent, §4 Tier-3 / P3) has what it needs; it is a
  conflict *value*, not lost work.
- **Future work.** Driving a real per-language build + test loop (the Sentinel
  integration the doc names) instead of a configured command; carrying richer
  Tier-3 provenance (which agent, which spec, embedding-derived context, §4
  Tier-3) into the demoted term; and hosting the demotion above the
  definition-level (tree-sitter) admission boundary once the Tier-2 tree-equality
  / I11 kernel checks are wired in.
