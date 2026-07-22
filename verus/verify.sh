#!/usr/bin/env bash
# Machine-check the omoplata-algebra kernel invariants with Verus.
#
# This runs the Verus binary over the verified model in this directory and
# exits non-zero if any theorem fails to verify. It is intentionally NOT part
# of `cargo build/test` — the model lives outside the cargo workspace and is
# checked only by this script (locally or by the `verus-verification` CI job).
#
# Configuration (env, with sensible defaults):
#   VERUS_BIN       path to the `verus` binary
#   VERUS_Z3_PATH   path to the Z3 4.12.5 binary Verus should use
#
# See verus/README.md for how to build Verus in this environment.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

VERUS_BIN="${VERUS_BIN:-verus}"
# If VERUS_Z3_PATH is unset, Verus falls back to the z3 next to its binary.
export VERUS_Z3_PATH="${VERUS_Z3_PATH:-}"

if ! command -v "$VERUS_BIN" >/dev/null 2>&1 && [ ! -x "$VERUS_BIN" ]; then
  echo "error: verus binary not found (set VERUS_BIN); see verus/README.md" >&2
  exit 127
fi

model="$here/omoplata_algebra_model.rs"
echo "== verus $("$VERUS_BIN" --version 2>/dev/null | tr '\n' ' ')"
echo "== z3   ${VERUS_Z3_PATH:-<bundled>}"
echo "== checking $model"

# --verify-root: this is a standalone file, not a crate.
"$VERUS_BIN" "$model"
echo "verus: all theorems verified (I1b round-trip; I10 disjoint commutation)"
