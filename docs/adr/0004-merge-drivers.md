# ADR-0004: Tier-2 merge drivers — Rust structural + Mergiraf shell-out (line fallback when absent)

- Status: Accepted
- Date: 2026-07-22
- Updated: 2026-07-22 — Mergiraf is now integrated as a real, PATH-detected
  shell-out driver (`MergirafDriver`), replacing the built-in line stand-in for
  supported non-Rust files. The built-in `LineDriver` remains the fallback used
  only when the `mergiraf` binary is absent or the extension is unsupported.

## Context
The design doc's merge pipeline (§4) escalates surviving conflicts to a
per-language **Tier-2 structural** driver, and names the interim/fallback driver
explicitly. From §8 scope:

> Tier-2 structural merge for **Rust only** (one grammar, dogfooded on the
> Autumn stack), **Mergiraf as the fallback driver for everything else**.

and the §4 architecture diagram lists an "Interim driver: Mergiraf" among the
untrusted proposers. The crate table (§7) marks `omoplata-drivers` as
**Untrusted by design**: drivers are *proposers* under the LCF architecture
(principle **P1**). Their output is a candidate merge that the verified kernel
admits only after checking tree equality and **trivia conservation (I11)**;
a bad or failed driver can only produce a rejected proposal or an honest
conflict, never a silently wrong merge.

Two facts shape this milestone (M5):

1. **Mergiraf is an external tool with a GPL license and an unstable library
   API.** It ships as a Rust crate, but (a) it is **GPL-3.0-only**, so linking
   it into `omoplata-drivers` would force the whole workspace's license posture
   to the GPL, and (b) its library API is explicitly documented as unstable and
   not for external consumers. Its **command-line interface is the stable,
   supported contract**. It is also not guaranteed to be present in every
   environment, so a hard dependency (crate *or* binary) would make the crate
   unbuildable/untestable where it is absent.
2. **tree-sitter is error-tolerant.** It recovers from malformed input and
   still returns a best-effort tree with `ERROR` / `MISSING` nodes, so a naive
   structural driver would happily merge partially-parsed trees.

## Decision
`omoplata-drivers` ships three drivers behind a `MergeDriver` trait, selected by
file extension (`select_driver`):

- **`RustStructuralDriver` (`"rust-structural"`)** — the Tier-2 structural merge
  for Rust, the point of M5. It merges at **definition granularity** using the
  tree-sitter extraction and tiered identity matcher from `omoplata-identity`
  (P6): base/left/right are split into top-level items (+ the inter-item text,
  so reassembly is byte-faithful), items are paired across versions by identity,
  and the merged item set is assembled in a documented canonical order
  (surviving base items in base order, then left-added, then right-added). A
  definition edited on both sides is line-merged internally via
  `omoplata_algebra::merge3`; unresolvable cases degrade to a first-class
  `Conflict` value. This succeeds where a pure line merge conflicts — e.g. two
  branches each appending a new item at the same textual location.

- **`MergirafDriver` (`"mergiraf"`)** — a **PATH-detected shell-out** to the
  external [Mergiraf](https://mergiraf.org) tool, the Tier-2 structural fallback
  for the 45+ non-Rust languages it supports (Java, Go, JSON, YAML, TOML,
  JS/TS, Python, C/C++, …). It writes base/left/right into a per-call tempdir
  (named with the real extension so Mergiraf picks the right grammar) and runs
  `mergiraf merge <base> <left> <right> -p <path> -o <out>` with stable marker
  labels (`-s base -x left -y right`) and a `-t` timeout. Exit 0 is a clean
  merge; exit 1 leaves diff3-style markers in the output, which the driver parses
  into first-class `Conflict` values (the source of truth). Any other outcome
  (missing binary, spawn failure, abnormal exit) surfaces as a `DriverError` so a
  misbehaving tool is *visible*, not silently degraded.

- **`LineDriver` (`"line"`)** — a diff3-style line merge wrapping the verified
  `omoplata_algebra::merge3`, used for a non-`.rs` path when Mergiraf is **absent
  from `PATH`** or the extension is not one Mergiraf supports.

### Mergiraf: integrated as a shell-out, not linked (why)
`MergirafDriver` earlier stood in as the built-in `LineDriver`; it **is now the
real Mergiraf**, wired as a subprocess. We shell out to the `mergiraf` binary
rather than depending on the crate for two reasons stated in Context: Mergiraf
is **GPL-3.0-only** (a process boundary keeps it a program we *invoke*, not a
library we *incorporate*, so it does not relicense the workspace), and its
**library API is unstable** while its CLI is the supported contract.

The process boundary is also the right **trust posture**. A Tier-2 driver is an
*untrusted proposer* (P1); Mergiraf additionally parses attacker-influenceable
input coming from an untrusted proposer. Running it out-of-process across a
filesystem boundary means a crash or misbehaviour in the child is an exit status
we observe, not memory we share. Its output remains a *candidate* the verified
kernel still gates.

**No hard dependency.** `mergiraf` is optional. A cached (`OnceLock`)
`mergiraf --version` probe (`mergiraf_available()`) decides at selection time
whether a supported non-Rust path uses `MergirafDriver` or falls back to the
built-in `LineDriver`. The crate therefore stays buildable and testable with no
external tool present — the integration tests that exercise the real binary are
guarded and skip when it is absent. This preserves the original
no-hard-dependency guarantee while delivering the real tool where it exists.

### Parse fallback for the structural driver
Because tree-sitter recovers from malformed input, the structural driver checks
that all three sides parse cleanly (no error nodes) via the new
`omoplata_identity::parses_cleanly` helper. If any side is malformed — or a hard
grammar/parse error occurs — it **falls back to `LineDriver`** and returns that
output (whose `driver` field is honestly `"line"`), rather than structurally
merging a broken tree. This keeps the driver safe on partial/invalid sources.

## Consequences
- **Untrusted, unverified, no kernel check yet.** These drivers sit outside the
  verified boundary by design. This crate does not yet host the kernel admission
  check (tree equality + I11 trivia conservation) that would gate a structural
  proposal in the full system; that wiring is a later milestone. The driver's
  own discipline is the I8-style honest-degradation rule: every result is a
  clean merge or a `Conflict` value, never a silent drop or silent side-pick.
- **A new public helper in `omoplata-identity`.** `parses_cleanly` was added to
  expose tree-sitter's error state (which `extract_definitions` swallows), so
  the driver can detect malformed input and degrade. It is additive and does not
  change existing behavior.
- **Trivia placement is approximate at v1.** Inter-item text (blank lines, free
  comments, doc comments not owned by an item's node) is preserved positionally
  during reassembly, but the design doc's full Roslyn-style trivia-ownership
  policy and the kernel's I11 conservation check are not yet implemented here.
  This matches the doc's staging (§4 Tier 2, Q2) and is a documented gap.
- **Mergiraf is integrated (was deferred).** Detecting `mergiraf` on `PATH` and
  shelling out is now implemented as `MergirafDriver` behind the same
  `MergeDriver` trait; the built-in `LineDriver` fallback still works (and is
  used) without it. Selection is extension-based, so grammars Mergiraf keys off a
  *bare filename* (e.g. `Makefile`, `go.mod`, `CMakeLists.txt`) route to the line
  fallback — a documented, safe conservatism.
- **Structural merges are "provisional/downgraded" at the CLI's kernel gate.**
  `omo merge-file`'s M9 kernel-certify step uses the *line* kernel, which cannot
  witness a structural (Mergiraf or Rust) merge and may downgrade a clean
  structural result to a conflict / non-zero exit. This is expected: the driver's
  merged text is still emitted, and callers assert on merged content, not the
  exit code. Hosting a structural kernel admission check is later work.
