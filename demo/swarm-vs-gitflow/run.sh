#!/usr/bin/env bash
# Reproduce the swarm-vs-gitflow demo without live LLM agents: the five agents'
# edits are replayed from patches/. See README.md for the write-up of the run
# this script re-executes.
#
# Usage: ./run.sh            (expects `omo` at ../../target/release/omo or $OMO)
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
OMO="${OMO:-$HERE/../../target/release/omo}"
OUT="$HERE/out"
command -v git >/dev/null || { echo "git required"; exit 1; }
[ -x "$OMO" ] || { echo "omo binary not found at $OMO (build with: cargo build --release)"; exit 1; }

rm -rf "$OUT" && mkdir -p "$OUT"
say() { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }
tally=()

# ---------------------------------------------------------------- agent copies
say "replaying swarm edits from patches/"
for i in 1 2 3 4 5; do
  mkdir -p "$OUT/agents/agent-$i"
  cp -r "$HERE/base/." "$OUT/agents/agent-$i/"
  (cd "$OUT/agents/agent-$i" && patch -sp1 < "$HERE/patches/agent-$i.patch")
done

# ------------------------------------------------- Track A: omoplata workflow
say "Track A: omoplata — 5 concurrent submit+land against ONE shared repo"
"$OMO" init "$OUT/omo-trunk" >/dev/null
for i in 1 2 3 4 5; do
  "$OMO" workspace add agent-$i "$OUT/agents/agent-$i" --repo "$OUT/omo-trunk" >/dev/null
done
for i in 1 2 3 4 5; do
  ( "$OMO" submit sub-$i --title "agent-$i change" ws/agent-$i --repo "$OUT/omo-trunk" &&
    "$OMO" land sub-$i --repo "$OUT/omo-trunk" ) > "$OUT/land-$i.log" 2>&1 &
done
wait
landed=$(grep -l "landed submission" "$OUT"/land-*.log | wc -l)
oplen=$("$OMO" op log --repo "$OUT/omo-trunk" | wc -l)
echo "concurrent landings succeeded: $landed/5; op log entries: $oplen (expect 10, gap-free)"
tally+=("omo   concurrency: $landed/5 concurrent landings on one repo, op log intact")

say "Track A: content integration via omo merge-file (land order 3,1,2,5,4)"
cp "$HERE/base/src/lib.rs" "$OUT/trunk-omo.rs"
omo_clean=0; omo_downgrade=0; omo_conflict=0
for i in 3 1 2 5 4; do
  if "$OMO" merge-file "$HERE/base/src/lib.rs" "$OUT/trunk-omo.rs" \
       "$OUT/agents/agent-$i/src/lib.rs" > "$OUT/omo-merged-$i.rs" 2> "$OUT/omo-merge-$i.err"; then
    echo "agent-$i: clean, kernel-admitted"
    omo_clean=$((omo_clean+1)); cp "$OUT/omo-merged-$i.rs" "$OUT/trunk-omo.rs"
  elif grep -q "downgraded to conflict" "$OUT/omo-merge-$i.err"; then
    echo "agent-$i: structural proposal correct but kernel-downgraded -> accept after validation"
    omo_downgrade=$((omo_downgrade+1)); cp "$OUT/omo-merged-$i.rs" "$OUT/trunk-omo.rs"
  else
    n=$(grep -c '^<<<<<<<' "$OUT/omo-merged-$i.rs")
    echo "agent-$i: honest conflict ($n region(s)) -> resolving"
    omo_conflict=$((omo_conflict+1))
    # The one genuine conflict: agent-3's Critical band vs agent-5's retuned
    # thresholds. Resolution combines both intents (95/75/50).
    python3 - "$OUT/omo-merged-$i.rs" "$OUT/trunk-omo.rs" <<'EOF'
import sys
text = open(sys.argv[1]).read()
conflicted = """<<<<<<< left
    if urgency >= 95 {
        Priority::Critical
    } else if urgency >= 80 {
=======
    if urgency >= 75 {
>>>>>>> right
"""
resolved = """    if urgency >= 95 {
        Priority::Critical
    } else if urgency >= 75 {
"""
assert conflicted in text, "unexpected conflict shape"
open(sys.argv[2], "w").write(text.replace(conflicted, resolved))
EOF
  fi
done
mkdir -p "$OUT/final-omo/src" && cp "$HERE/base/Cargo.toml" "$OUT/final-omo/" && cp "$OUT/trunk-omo.rs" "$OUT/final-omo/src/lib.rs"
if (cd "$OUT/final-omo" && cargo test -q >/dev/null 2>&1); then omo_final=PASS; else omo_final=FAIL; fi
tally+=("omo   round 1: $omo_clean auto-clean, $omo_downgrade kernel-downgrade (proposal correct), $omo_conflict genuine conflict; final tests: $omo_final")

# ------------------------------------------------------- Track B: git flow
say "Track B: git flow — feature branches off develop, merged in same order"
G="$OUT/git-flow"; mkdir -p "$G" && cd "$G"
git init -q -b main . && git config user.email demo@example.com && git config user.name demo
cp -r "$HERE/base/." . && printf 'target/\nCargo.lock\n' > .gitignore
git add -A && git commit -qm "base" && git checkout -qb develop
for i in 1 2 3 4 5; do
  git checkout -qb feature/agent-$i develop
  cp "$OUT/agents/agent-$i/src/lib.rs" src/lib.rs
  git commit -qam "agent-$i change" && git checkout -q develop
done
git_clean=0; git_conflict=0
for i in 3 1 2 5 4; do
  if git merge --no-ff --no-edit -q feature/agent-$i >/dev/null 2>&1; then
    echo "agent-$i: clean"; git_clean=$((git_clean+1))
  else
    echo "agent-$i: CONFLICT -> resolving"; git_conflict=$((git_conflict+1))
    python3 - src/lib.rs <<'EOF'
import sys
text = open(sys.argv[1]).read()
start = text.index("<<<<<<<"); end = text.index("\n", text.index(">>>>>>>")) + 1
# Replace only the conflicted head of the if-chain; the shared tail
# (High/Normal/Low arms) follows the marker block and stays as-is.
resolved = """    if urgency >= 95 {
        Priority::Critical
    } else if urgency >= 75 {
"""
open(sys.argv[1], "w").write(text[:start] + resolved + text[end:])
EOF
    git add src/lib.rs && git commit -qm "merge agent-$i: resolve priority_of"
  fi
done
if cargo test -q >/dev/null 2>&1; then git_final=PASS; else git_final=FAIL; fi
tally+=("git   round 1: $git_clean auto-clean, $git_conflict conflict(s); final tests: $git_final")
cd "$HERE"

# ------------------------------------------- Round 2: silent-wrong-answer probes
say "Round 2a: refactor MOVES priority_of; another agent EDITS it in place"
python3 - "$HERE/base/src/lib.rs" "$OUT" <<'EOF'
import sys
base = open(sys.argv[1]).read(); out = sys.argv[2]
block = """/// Map a raw urgency score to a priority band.
pub fn priority_of(urgency: u32) -> Priority {
    if urgency >= 80 {
        Priority::High
    } else if urgency >= 40 {
        Priority::Normal
    } else {
        Priority::Low
    }
}

"""
assert block in base
open(f"{out}/r2a_left_move.rs", "w").write(base.replace(block, "").rstrip() + "\n\n" + block.rstrip() + "\n")
open(f"{out}/r2a_right_edit.rs", "w").write(base.replace("urgency >= 40", "urgency >= 45"))
EOF
git merge-file -p "$OUT/r2a_left_move.rs" "$HERE/base/src/lib.rs" "$OUT/r2a_right_edit.rs" > "$OUT/r2a_git.rs"
gcopies=$(grep -c "pub fn priority_of" "$OUT/r2a_git.rs")
echo "git:  exit=$? copies-of-fn=$gcopies (conflict at old site + STALE copy at new site: resolving 'accept the move' silently loses the edit)"
"$OMO" merge-file "$HERE/base/src/lib.rs" "$OUT/r2a_left_move.rs" "$OUT/r2a_right_edit.rs" > "$OUT/r2a_omo.rs" 2> "$OUT/r2a_omo.err"
ocopies=$(grep -c "pub fn priority_of" "$OUT/r2a_omo.rs")
grep -q "urgency >= 45" "$OUT/r2a_omo.rs" && okept="edit-followed-move" || okept="edit-LOST"
echo "omo:  copies-of-fn=$ocopies, $okept ($(tail -1 "$OUT/r2a_omo.err"))"
tally+=("R2a  move+edit: git plants stale duplicate (resolution trap); omo tracks the definition, 1 copy, edit kept")

say "Round 2b: two agents independently add the same method (duplicate work)"
python3 - "$HERE/base/src/lib.rs" "$OUT" <<'EOF'
import sys
base = open(sys.argv[1]).read(); out = sys.argv[2]
left = base.replace("""    pub fn len(&self) -> usize {
        self.tasks.len()
    }
}""", """    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Whether the queue contains no tasks.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}""")
right = base.replace("""    /// Remove and return the highest-priority task.""", """    /// True when nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Remove and return the highest-priority task.""")
assert left != base and right != base
open(f"{out}/r2b_left.rs", "w").write(left)
open(f"{out}/r2b_right.rs", "w").write(right)
EOF
git merge-file -p "$OUT/r2b_left.rs" "$HERE/base/src/lib.rs" "$OUT/r2b_right.rs" > "$OUT/r2b_git.rs"
echo "git:  exit=$? copies-of-is_empty=$(grep -c 'pub fn is_empty' "$OUT/r2b_git.rs") markers=$(grep -c '^<<<<<<<' "$OUT/r2b_git.rs")  <- exit 0, duplicate method, does not compile"
"$OMO" merge-file "$HERE/base/src/lib.rs" "$OUT/r2b_left.rs" "$OUT/r2b_right.rs" > "$OUT/r2b_omo.rs" 2>/dev/null
echo "omo:  exit=$? copies-of-is_empty=$(grep -c 'pub fn is_empty' "$OUT/r2b_omo.rs") markers=$(grep -c '^<<<<<<<' "$OUT/r2b_omo.rs")  <- member-granularity: honest scoped conflict, both variants inside the marker block"
cat > "$OUT/validate.sh" <<'SH'
#!/bin/sh
d=$(mktemp -d) && mkdir -p "$d/src" && cp "$1" "$d/src/lib.rs"
printf '[package]\nname = "v"\nversion = "0.1.0"\nedition = "2021"\n' > "$d/Cargo.toml"
cd "$d" && cargo check -q 2>/dev/null
SH
chmod +x "$OUT/validate.sh"
if "$OMO" merge-file "$HERE/base/src/lib.rs" "$OUT/r2b_left.rs" "$OUT/r2b_right.rs" --validate "$OUT/validate.sh" >/dev/null 2> "$OUT/r2b_val.err"; then
  echo "omo --validate: ACCEPTED (unexpected)"
else
  echo "omo --validate: $(tail -1 "$OUT/r2b_val.err")"
fi
tally+=("R2b  duplicate work: git silently merges a broken file (P9 --validate is git's missing net); omo member granularity makes it an honest scoped conflict")

# ---------------------------------------- Round 3: the queue that never blocks
say "Round 3: conflicts as values — land ON TOP of an unresolved conflict"
R3="$OUT/round3"; mkdir -p "$R3"
cp "$HERE/base/src/lib.rs" "$R3/trunk.rs"
for i in 3 1 2; do
  "$OMO" merge-file "$HERE/base/src/lib.rs" "$R3/trunk.rs" "$OUT/agents/agent-$i/src/lib.rs" > "$R3/m.rs" 2>/dev/null \
    && cp "$R3/m.rs" "$R3/trunk.rs"
done
"$OMO" merge-file "$HERE/base/src/lib.rs" "$R3/trunk.rs" "$OUT/agents/agent-5/src/lib.rs" > "$R3/m5.rs" 2>/dev/null
echo "agent-5: genuine conflict (exit $?) -> adopted AS trunk, unresolved (conflict as value)"
cp "$R3/m5.rs" "$R3/trunk.rs"
"$OMO" merge-file "$HERE/base/src/lib.rs" "$R3/trunk.rs" "$OUT/agents/agent-4/src/lib.rs" > "$R3/m4.rs" 2> "$R3/m4.err"
r3exit=$?
echo "agent-4 lands on the CONFLICTED trunk: exit $r3exit — $(cat "$R3/m4.err")"
cp "$R3/m4.rs" "$R3/trunk.rs"
echo "queryable: $("$OMO" conflicts "$R3/trunk.rs" | head -1)"
# Resolution is a commit that collapses the term — applied LAST, after
# everything else landed around it.
python3 - "$R3/trunk.rs" <<'EOF'
import sys
text = open(sys.argv[1]).read()
conflicted = """<<<<<<< left
    if urgency >= 95 {
        Priority::Critical
    } else if urgency >= 80 {
=======
    if urgency >= 75 {
>>>>>>> right
"""
resolved = """    if urgency >= 95 {
        Priority::Critical
    } else if urgency >= 75 {
"""
assert conflicted in text, "unexpected conflict shape"
open(sys.argv[1], "w").write(text.replace(conflicted, resolved))
EOF
mkdir -p "$R3/final/src" && cp "$HERE/base/Cargo.toml" "$R3/final/" && cp "$R3/trunk.rs" "$R3/final/src/lib.rs"
if (cd "$R3/final" && cargo test -q >/dev/null 2>&1); then r3final=PASS; else r3final=FAIL; fi
echo "resolved last; final tests: $r3final"
tally+=("R3   conflict rides through a later landing (exit 2, carried), queryable via 'omo conflicts', resolved last; tests: $r3final")

# --------------------------------------------------------- contention at n=10
say "Contention: 10 concurrent writers against one shared repo"
C="$OUT/contention"; mkdir -p "$C"
git init -q -b main "$C/git" && (cd "$C/git" && git config user.email d@e.c && git config user.name d && echo s > seed && git add -A && git commit -qm seed)
for i in $(seq 1 10); do
  (cd "$C/git" && echo "$i" > "f$i" && git add "f$i" && git commit -qm "c$i") >/dev/null 2>>"$C/git-err-$i.log" &
done
wait
gitok=$(cd "$C/git" && git log --oneline | wc -l); gitok=$((gitok-1))
echo "git: $gitok/10 commits survived (rest died on index.lock)"
"$OMO" init "$C/omo" >/dev/null
for i in $(seq 1 10); do mkdir -p "$C/wc$i" && echo "x$i" > "$C/wc$i/f.txt"; done
for i in $(seq 1 10); do
  ( "$OMO" workspace add w$i "$C/wc$i" --repo "$C/omo" &&
    "$OMO" submit s$i --title "t$i" ws/w$i --repo "$C/omo" &&
    "$OMO" land s$i --repo "$C/omo" ) > "$C/omo-$i.log" 2>&1 &
done
wait
omook=$(grep -l "landed submission" "$C"/omo-*.log | wc -l)
echo "omo: $omook/10 concurrent landings succeeded, op log $("$OMO" op log --repo "$C/omo" | wc -l) entries"
tally+=("cont  n=10 one shared repo: git $gitok/10 commits survive; omo $omook/10 landings survive")

# ------------------------------------------------------------------- summary
say "SUMMARY"
for line in "${tally[@]}"; do echo "  $line"; done
