# Machine-checked model of `omoplata-algebra` (Verus)

This directory holds a [Verus](https://github.com/verus-lang/verus)-verified
model of the `omoplata-algebra` value layer. It is the machine-checked twin of
the algorithm shape in `crates/omoplata-algebra/src/{patch.rs,commute.rs}` and
discharges the design doc's highest-value kernel invariants as proofs, not just
property tests.

It is **outside the cargo workspace on purpose**: the root `Cargo.toml` uses an
explicit `members` list that does not include `verus/`, so `cargo build`,
`cargo test --all`, and `cargo clippy` never compile these files. They are
checked only by the Verus binary via `verify.sh` (locally, or the isolated
`verus-verification` CI job).

## What is proven

`omoplata_algebra_model.rs` — verified as **`7 verified, 0 errors`**, with no
`assume` / `admit` / `external_body` anywhere:

| Invariant | Theorem | Status |
|-----------|---------|--------|
| **I1b** diff faithfulness (round-trip) | `i1b_roundtrip`: `apply(base, diff(base,target)) == target` | fully proven |
| **I10** disjoint-support commutation (the enabling lemma the doc says makes I5 "fall out") | `i10_disjoint_commute`: disjoint-support patches commute | proven for the **length-preserving** core |

`Doc` is modeled as `Seq<int>` — one opaque integer token per line. The algebra
never inspects a line's bytes (`diff`/`apply`/`commute` compare whole lines with
`==`), so token equality is exactly the equality the real algorithm uses; the
abstraction is faithful because line-internal structure is invisible to the
value layer.

### Scope, stated honestly

- **I1b is fully machine-checked** on a faithful single-hunk diff (common-prefix
  context preserved, remaining suffix replaced) through an `apply` model that
  carries the same range check and context check as production `apply`. The diff
  need not be *minimal* (minimality is I1a, a separate invariant); it is
  *faithful* (round-trips), which is what I1b asserts.
- **I10 is proven for the length-preserving disjoint case** — the composable
  core in which the doc's non-rebased statement
  `apply(apply(base,p),q) == apply(apply(base,q),p)` literally holds, because no
  hunk shifts the other's coordinates. The **general length-changing I5**
  (which needs the coordinate-rebase machinery in `commute.rs`) is **not yet
  discharged in Verus**; it remains guarded by the `disjoint_commutes_both_orders`
  proptest. See ADR-0003.
- The **shipping** `diff`/`apply`/`commute` are *not themselves* Verus-verified.
  They stay trusted-by-testing (proptests `diff_apply_roundtrip`,
  `disjoint_commutes_both_orders`). This module is their machine-checked
  companion and a differential oracle for the same algorithm shape.

## Running the proofs

```bash
# with a Verus binary and Z3 4.12.5 on hand:
VERUS_BIN=/path/to/verus VERUS_Z3_PATH=/path/to/z3 ./verus/verify.sh
```

`verify.sh` exits non-zero if any theorem fails. `VERUS_BIN` defaults to `verus`
on `PATH`; `VERUS_Z3_PATH` defaults to the Z3 bundled next to the Verus binary.

## Building Verus in this environment

Verus `0.2026.07.21.1beb0fa` (Z3 4.12.5, Rust toolchain 1.96.0) builds cleanly
here. The one host that returns 403 from this environment is the **Z3 GitHub
releases** page; that is bypassed by installing the *identical* pinned Z3 binary
from PyPI (`z3-solver`). `git clone` and `static.rust-lang.org` work.

```bash
git clone https://github.com/madmax983/verus verus-src
cd verus-src
TC=$(grep -o '1\.[0-9]*\.[0-9]*' rust-toolchain.toml | head -1)
rustup component add rustc-dev rustfmt llvm-tools \
  --toolchain ${TC}-x86_64-unknown-linux-gnu
ZV=$(grep -oE '4\.[0-9]+\.[0-9]+' source/tools/get-z3.sh | head -1)   # 4.12.5
pip install z3-solver==${ZV}          # ships the exact pinned z3 binary
Z3BIN=$(which z3); "$Z3BIN" --version  # Z3 version 4.12.5
cd source && unset CARGO_TARGET_DIR && export VERUS_Z3_PATH="$Z3BIN"
source ../tools/activate               # run bare, not piped
export PATH="$PWD/../tools/vargo/target/release:$PATH"
vargo build --release                  # ~6 min; ends "vstd ... verified, 0 errors"
# verus binary: source/target-verus/release/verus
```

On a normal CI runner (with unrestricted internet), the Z3 GitHub-release 403 is
not a factor; the `verus-verification` job in `.github/workflows/ci.yml` uses the
same PyPI-Z3 path anyway for reproducibility.
