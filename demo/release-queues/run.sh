#!/usr/bin/env bash
# Release lines as landing queues (ADR-0009): a reproducible walkthrough.
# See README.md for the narrative. Expects `omo` at ../../target/release/omo
# (or $OMO).
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
OMO="${OMO:-$HERE/../../target/release/omo}"
OUT="$HERE/out"
[ -x "$OMO" ] || { echo "omo binary not found at $OMO (build with: cargo build --release)"; exit 1; }

rm -rf "$OUT" && mkdir -p "$OUT" && cd "$OUT"
say() { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }

"$OMO" init repo >/dev/null

# The release queue's P9 validator: run each materialized change's test suite
# (the batch layout is sub-<id>/change-<i>, so walk for crate roots).
cat > regression.sh <<'SH'
#!/bin/sh
set -e
for toml in $(find "$1" -name Cargo.toml); do
  (cd "$(dirname "$toml")" && cargo test -q >/dev/null 2>&1) || exit 1
done
SH
chmod +x regression.sh

say "two landing targets: the implicit trunk, and a strict release line"
"$OMO" queue add release-1.2 --validate "$OUT/regression.sh {}" \
  --description "the 1.2 release line" --repo repo
"$OMO" queue list --repo repo

# A hotfix workspace with a passing crate.
mkdir -p wc/src
cat > wc/Cargo.toml <<'EOF'
[workspace]

[package]
name = "app"
version = "0.1.0"
edition = "2021"
EOF
cat > wc/src/lib.rs <<'EOF'
pub fn parse(input: &str) -> usize {
    input.len()
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses() {
        assert_eq!(super::parse("ab"), 2);
    }
}
EOF
"$OMO" workspace add hotfix wc --repo repo >/dev/null

say "gate 1 — approval: a pending submission cannot land in the release line"
"$OMO" submit sub-207 --title "Fix length parsing" ws/hotfix --pending --repo repo
"$OMO" land sub-207 --queue release-1.2 --repo repo 2>&1 || true
"$OMO" approve sub-207 --by kara --repo repo

say "gate 2 — P9 validation: approved content lands only after the suite passes"
"$OMO" land sub-207 --queue release-1.2 --repo repo

say "backport = the SAME change landing in a second queue (identity preserved)"
"$OMO" land sub-207 --repo repo
"$OMO" ref list --repo repo | grep public

say "a broken change is refused in-band: nothing transitions, nothing to revert"
cat > wc/src/lib.rs <<'EOF'
pub fn parse(input: &str) -> usize {
    input.len() + 1
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses() {
        assert_eq!(super::parse("ab"), 2);
    }
}
EOF
"$OMO" submit sub-208 --title "Off-by-one tweak" ws/hotfix --repo repo
"$OMO" land sub-208 --queue release-1.2 --repo repo 2>&1 || true

say "gate 3 — carried conflict values (§5.4): trunk lands them, the release line refuses"
cat > wc/src/lib.rs <<'EOF'
pub fn parse(input: &str) -> usize {
<<<<<<< left
    input.len()
=======
    input.chars().count()
>>>>>>> right
}
EOF
"$OMO" conflicts wc/src/lib.rs || true
"$OMO" submit sub-209 --title "Unicode-aware length (conflicted)" ws/hotfix --repo repo
"$OMO" land sub-209 --repo repo
"$OMO" land sub-209 --queue release-1.2 --repo repo 2>&1 || true

say "Tier-0 batch: disjoint submissions validate as one and land together"
mkdir -p wa wb
echo "feature a" > wa/a.txt
echo "feature b" > wb/b.txt
"$OMO" workspace add wa wa --repo repo >/dev/null
"$OMO" workspace add wb wb --repo repo >/dev/null
"$OMO" submit sub-a --title "feature a" ws/wa --repo repo >/dev/null
"$OMO" submit sub-b --title "feature b" ws/wb --repo repo >/dev/null
"$OMO" land sub-a sub-b --repo repo

say "overlap refuses the WHOLE batch, naming the colliding paths"
mkdir -p wc2 && echo "feature a, differently" > wc2/a.txt
"$OMO" workspace add wc2 wc2 --repo repo >/dev/null
"$OMO" submit sub-c --title "collides with a" ws/wc2 --repo repo >/dev/null
"$OMO" submit sub-a2 --title "a again" ws/wa --repo repo >/dev/null
"$OMO" land sub-c sub-a2 --repo repo 2>&1 || true

say "backport: approval carried forward under an identity certificate"
"$OMO" backport sub-a --to release-1.2 --repo repo

say "what still needs backporting is a revset, not a branch comparison"
echo "landed(trunk) & ~landed(release-1.2):"
"$OMO" revset 'landed(trunk) & ~landed(release-1.2)' --repo repo

say "DONE — policy is the queue object; the release line never saw a bad landing"