//! Auto-rebase over conflict values — the working-model half of "conflicts are
//! values" (design doc §5.4, principle **P3**, invariant **I4**).
//!
//! # What the design doc asks for
//!
//! **§5.4 — Conflicts as values:**
//!
//! > A conflict is a stored term: `Conflict{base, sides: [tree], provenance}`
//! > embedded in a commit's tree. **Rebase maps over conflicts; resolution is a
//! > commit that collapses the term.** Confluence (I4) guarantees resolution
//! > timing is irrelevant to the final tree.
//!
//! **§3 P3 — Conflicts are values:**
//!
//! > Merges and rebases never fail and never block; a conflicted state
//! > propagates through descendant rebases and is resolved by a later commit,
//! > whenever convenient. omoplata additionally *proves* conflict confluence: a
//! > conflict plus its resolution normalizes to the same tree regardless of when
//! > resolution occurs.
//!
//! **§3 P3 (fleet context) / P4:** "review discipline is the binding constraint";
//! async conflict resolution is what keeps concurrent agents unblocked — a rebase
//! that hit a conflict must carry it forward as a value rather than stop the
//! world.
//!
//! **§5.3 — stacking is a property of changes:** "stacking are properties of
//! changes, not commits"; a stack of successive versions can be replayed onto a
//! new base, each step threaded onto the previous step's rebased result, with
//! conflicts accumulating as values rather than aborting the stack.
//!
//! **I4 — Conflict confluence (elevation target):** "for any conflict with
//! resolution, normalization commutes with rebase". Soundness does not wait on
//! this proof (I12 guards each instance at runtime); the theorem upgrades the
//! per-instance check to universal certainty.
//!
//! # What this module builds
//!
//! This is the line-layer realisation of §5.4 on top of the existing algebra
//! ([`merge3`], [`Conflict`], [`Merge`], [`diff`]/[`apply`], and the [`kernel`]):
//!
//! * [`rebase`] — replay my change on top of a sibling `onto` change that shares
//!   the same base. Disjoint edits replay cleanly; overlaps emit **conflict
//!   values** rather than failing. This is the directional view of a three-way
//!   merge with `onto` as the new base.
//! * [`rebase_merge`] — the key §5.4 property: rebase a value that *already
//!   contains* conflicts. An unresolved [`Conflict`] carried in a [`Merge`]
//!   **survives** the rebase (the rebase maps over it); it is never silently
//!   dropped or "resolved" by the rebase.
//! * [`rebase_stack`] — auto-rebase a stack of successive versions (§5.3) onto a
//!   new base, threading each step's result into the next and accumulating
//!   conflicts as values, never aborting the whole stack on one conflict.
//! * [`resolve`] / [`resolve_all`] — resolution collapses the term (§5.4):
//!   replacing a conflict value with chosen lines removes it from the document.
//!
//! [`kernel`]: crate::kernel
//!
//! # Example
//!
//! ```
//! use omoplata_algebra::{rebase, Doc};
//!
//! let base = Doc::from_str("a\nb\nc\nd");
//! let mine = Doc::from_str("a\nB\nc\nd"); // I edit line 1
//! let onto = Doc::from_str("a\nb\nc\nD"); // the branch edits line 3
//!
//! // My edit replays cleanly on top of the independent `onto` change.
//! let r = rebase(&base, &mine, &onto);
//! assert!(r.clean);
//! assert_eq!(r.result, Doc::from_str("a\nB\nc\nD"));
//! ```

use crate::doc::Doc;
use crate::merge::{merge3, Conflict, Merge, CONFLICT_END, CONFLICT_SEP, CONFLICT_START};
use crate::patch::{apply, Hunk, Patch};

/// Marker beginning a conflicted region in a rebased document: **my** side.
pub const REBASE_MINE: &str = "<<<<<<< mine";
/// Marker separating my side from the `onto` side in a rebased document.
pub const REBASE_SEP: &str = CONFLICT_SEP;
/// Marker ending a conflicted region in a rebased document: the **onto** side.
pub const REBASE_ONTO: &str = ">>>>>>> onto";

/// The result of rebasing a change: a best-effort document plus the conflicts
/// carried forward as values (design doc §5.4).
///
/// `result` is the merged document with conflicted spans rendered deterministically
/// as [`REBASE_MINE`] / [`REBASE_SEP`] / [`REBASE_ONTO`] marker blocks. The
/// authoritative data is always [`conflicts`](Rebased::conflicts): the rendering
/// is a human view derived from the values, never the other way round. `clean` is
/// exactly `conflicts.is_empty()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebased {
    /// The rebased document (with marker-rendered conflicts, if any).
    pub result: Doc,
    /// The conflicts carried forward as stored terms; empty iff the rebase is clean.
    pub conflicts: Vec<Conflict>,
    /// Whether the rebase produced no conflicts (`conflicts.is_empty()`).
    pub clean: bool,
}

impl Rebased {
    /// View this rebase result as a [`Merge`] so it can itself be rebased again
    /// via [`rebase_merge`] (the "rebase maps over conflicts" path, §5.4).
    #[must_use]
    pub fn as_merge(&self) -> Merge {
        Merge {
            merged: self.result.clone(),
            conflicts: self.conflicts.clone(),
        }
    }
}

/// Replay my change on top of a sibling `onto` change that shares the same base.
///
/// `mine` and `onto` both derive from `base`. The change `diff(base, mine)` is
/// replayed on top of `onto` (the new base): where my patch is independent of
/// `onto`'s change (`diff(base, onto)`, disjoint support) the replay is clean;
/// where they overlap, the overlap is emitted as a first-class [`Conflict`]
/// **value** rather than failing (design doc §3 P3: "merges and rebases never
/// fail and never block").
///
/// This is the directional view of a three-way merge with `onto` as the new base
/// — concretely [`merge3`]`(base, mine, onto)`. By the by-construction merge
/// symmetry (invariant **I2**) that is the same computation as the design doc's
/// `merge3(base, onto, mine)`; passing `mine` as the left side simply orients the
/// carried [`Conflict`] values so that `left` holds my replayed lines and `right`
/// holds `onto`'s, matching the [`REBASE_MINE`] / [`REBASE_ONTO`] rendering.
///
/// PROOF OBLIGATION (I8 — kernel admission, "no silent wrong answers"): every
/// span resolves to a clean one-sided/identical replay or a stored [`Conflict`]
/// value; an overlap is never silently collapsed to one side. Inherited from
/// [`merge3`] and guarded by the rebase tests.
#[must_use]
pub fn rebase(base: &Doc, mine: &Doc, onto: &Doc) -> Rebased {
    // Directional three-way merge: `mine` on the left so conflicts read
    // mine-vs-onto; onto on the right as the new base my edits land on.
    let m = merge3(base, mine, onto);
    let result = relabel_to_rebase_markers(&m.merged);
    Rebased {
        clean: m.conflicts.is_empty(),
        conflicts: m.conflicts,
        result,
    }
}

/// Rebase a value that already contains conflicts — the key §5.4 property,
/// "**Rebase maps over conflicts**".
///
/// `prior` is a [`Merge`] that may carry unresolved [`Conflict`] values (from an
/// earlier [`merge3`] or [`rebase`]). `onto_patch` is a further change to rebase
/// that value over, expressed as a [`Patch`] against `prior.merged`. The rebase
/// *maps over* each conflict: hunks of `onto_patch` that fall outside every
/// conflict region are applied (mapping the surrounding document forward), while
/// each unresolved conflict is **carried forward unchanged as a value** — never
/// silently dropped or "resolved" by the rebase.
///
/// Concretely: any hunk of `onto_patch` whose support overlaps a rendered
/// conflict region is withheld (the conflict wins and survives — honest
/// degradation, cf. §5.4's "resolution is a *commit*", not a side effect of
/// rebase); the remaining hunks are applied. The returned [`Merge`] carries the
/// same [`conflicts`](Merge::conflicts) as `prior`.
///
/// PROOF OBLIGATION (I4 — conflict confluence): this is the "rebase" side of the
/// commuting square `normalize ∘ rebase == rebase ∘ normalize`. An unresolved
/// conflict must map forward as a value so that resolving it before or after the
/// rebase yields the same final tree. Held per-instance here (the conflict is
/// preserved verbatim); the universal theorem is the design doc's elevation
/// target. Guarded by `maps_over_conflict_preserves_it` and the bounded
/// confluence test `resolve_then_rebase_equals_rebase_then_resolve`.
#[must_use]
pub fn rebase_merge(prior: &Merge, onto_patch: &Patch) -> Merge {
    let regions = conflict_marker_regions(&prior.merged);

    // Keep only the hunks that do not touch any conflict region: those map the
    // surrounding document forward. Hunks overlapping a conflict are withheld so
    // the conflict survives as a value (the rebase does not resolve it).
    let safe_hunks: Vec<Hunk> = onto_patch
        .hunks()
        .iter()
        .filter(|h| !overlaps_any_region(h, &regions))
        .cloned()
        .collect();
    let safe = Patch::from_hunks(safe_hunks);

    // `safe` is a sub-patch of a diff against `prior.merged`, so its context
    // matches by construction; if application ever failed we degrade honestly to
    // the un-advanced document rather than panic (the conflict still rides along).
    let merged = apply(&safe, &prior.merged).unwrap_or_else(|_| prior.merged.clone());

    Merge {
        merged,
        conflicts: prior.conflicts.clone(),
    }
}

/// Auto-rebase a stack of successive versions (§5.3) onto a new base.
///
/// `stack` is a sequence of successive documents `[v1, v2, …, vn]`, each derived
/// from the previous (with `base` as `v0`): the change of step `i` is
/// `diff(v(i-1), vi)`. Every step is replayed onto the growing rebased result and
/// its [`Rebased`] is collected, in stack order.
///
/// **Ordering / threading.** The stack is replayed front-to-back. Step `i` calls
/// [`rebase`]`(v(i-1), vi, acc)` where `acc` is the accumulated rebased document
/// (`onto` for the first step). The original pre-rebase version `v(i-1)` stays the
/// three-way base, while each step's `result` becomes the `onto` the next step
/// lands on — so the whole stack is threaded onto `onto` in order. Conflicts
/// accumulate as values: a conflicting step carries its conflict in its own
/// [`Rebased`] and the next step rebases onto that conflicted document, so **one
/// conflict never aborts the stack** (design doc §3 P3 / P4 — async resolution
/// keeps the fleet unblocked).
///
/// PROOF OBLIGATION (I4): each step preserves earlier steps' unresolved conflicts
/// as values (via [`rebase`]'s conflict handling), so resolution of any step can
/// be deferred past later steps without changing the eventual resolved tree.
/// Guarded by `rebase_stack_carries_a_conflicting_step`.
#[must_use]
pub fn rebase_stack(base: &Doc, stack: &[Doc], onto: &Doc) -> Vec<Rebased> {
    let mut out = Vec::with_capacity(stack.len());
    let mut prev = base.clone();
    let mut acc = onto.clone();
    for version in stack {
        let step = rebase(&prev, version, &acc);
        acc = step.result.clone();
        prev = version.clone();
        out.push(step);
    }
    out
}

/// Resolve one conflict by collapsing it to the chosen lines (design doc §5.4:
/// "resolution is a commit that collapses the term").
///
/// This models resolution as a pure function on a single [`Conflict`] value: the
/// chosen lines (typically one side, or a hand-authored reconciliation) *replace*
/// the conflict. After substituting the result into the document, the conflict
/// term is gone — see [`resolve_all`], which does the substitution over a whole
/// rendered document. The `conflict` argument is accepted so callers can inspect
/// `base`/`left`/`right` when choosing; the collapse itself is the chosen lines.
///
/// PROOF OBLIGATION (I4 / I12 — resolution admission): the resolved lines are the
/// value the conflict collapses to; confluence requires this collapse to be
/// independent of *when* it happens relative to rebases. Held per-instance (the
/// substitution is a pure replacement); guarded by
/// `resolve_then_rebase_equals_rebase_then_resolve`.
#[must_use]
pub fn resolve(conflict: &Conflict, chosen: &[String]) -> Vec<String> {
    // The collapse is the chosen lines. `conflict` is available for callers that
    // want to pick a side; we validate nothing here — any reconciliation the
    // author commits to is a legal resolution (§5.4).
    let _ = conflict;
    chosen.to_vec()
}

/// Resolve every conflict rendered in `rendered`, collapsing each term to the
/// corresponding chosen lines and producing a clean [`Doc`] with **no markers**.
///
/// `rendered` is a document containing marker-rendered conflict regions (either
/// [`rebase`]'s [`REBASE_MINE`]/[`REBASE_ONTO`] blocks or [`merge3`]'s
/// left/right blocks). The `resolutions` slice supplies the collapse for each
/// conflict region in document order: region `k` is replaced wholesale by
/// `resolutions[k]`. A region with no corresponding resolution (fewer
/// `resolutions` than regions) is left intact — its term is *not* collapsed, so
/// resolution can be partial and deferred (§3 P3).
///
/// When every region has a resolution the output contains no marker lines: the
/// conflict terms are gone (design doc §5.4). Guarded by
/// `resolve_all_collapses_the_term`.
#[must_use]
pub fn resolve_all(rendered: &Doc, resolutions: &[Vec<String>]) -> Doc {
    let lines = rendered.lines();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    let mut region = 0usize;
    while i < lines.len() {
        if is_conflict_start(&lines[i]) {
            // Find the matching end marker for this region.
            let mut j = i + 1;
            while j < lines.len() && !is_conflict_end(&lines[j]) {
                j += 1;
            }
            if j < lines.len() {
                // A well-formed region [i, j]. Collapse it if we have a resolution.
                if let Some(chosen) = resolutions.get(region) {
                    out.extend_from_slice(chosen);
                } else {
                    // No resolution supplied: keep the region intact (deferred).
                    out.extend_from_slice(&lines[i..=j]);
                }
                region += 1;
                i = j + 1;
                continue;
            }
            // Unterminated marker: treat as ordinary content (no region here).
        }
        out.push(lines[i].clone());
        i += 1;
    }
    Doc::from_lines(out)
}

/// Rewrite [`merge3`]'s left/right conflict markers into this module's
/// mine/onto markers, leaving all other lines untouched.
///
/// The separator is shared ([`REBASE_SEP`] == [`CONFLICT_SEP`]), so only the
/// start and end marker lines change. The structured [`Conflict`] values remain
/// the source of truth; this only affects the human-readable rendering.
fn relabel_to_rebase_markers(merged: &Doc) -> Doc {
    let lines = merged
        .lines()
        .iter()
        .map(|l| {
            if l == CONFLICT_START {
                REBASE_MINE.to_owned()
            } else if l == CONFLICT_END {
                REBASE_ONTO.to_owned()
            } else {
                l.clone()
            }
        })
        .collect();
    Doc::from_lines(lines)
}

/// Whether a line opens a conflict region, in either marker vocabulary.
fn is_conflict_start(line: &str) -> bool {
    line == CONFLICT_START || line == REBASE_MINE
}

/// Whether a line closes a conflict region, in either marker vocabulary.
fn is_conflict_end(line: &str) -> bool {
    line == CONFLICT_END || line == REBASE_ONTO
}

/// The half-open line intervals `[start, end)` occupied by rendered conflict
/// regions in `doc` (inclusive of the marker lines).
fn conflict_marker_regions(doc: &Doc) -> Vec<std::ops::Range<usize>> {
    let lines = doc.lines();
    let mut regions = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        if is_conflict_start(&lines[i]) {
            let mut j = i + 1;
            while j < lines.len() && !is_conflict_end(&lines[j]) {
                j += 1;
            }
            if j < lines.len() {
                regions.push(i..j + 1);
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    regions
}

/// Whether a hunk's support interval overlaps any conflict region. A pure
/// insertion (empty support) overlaps a region only when its anchor sits strictly
/// inside that region, so an insertion at a region boundary is still applied.
fn overlaps_any_region(h: &Hunk, regions: &[std::ops::Range<usize>]) -> bool {
    let s = h.support();
    regions.iter().any(|r| {
        if s.start == s.end {
            // Pure insertion: overlaps only if anchored strictly inside the region.
            s.start > r.start && s.start < r.end
        } else {
            s.start < r.end && r.start < s.end
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::diff;

    fn doc(lines: &[&str]) -> Doc {
        Doc::from_lines(lines.iter().map(|s| (*s).to_owned()).collect())
    }

    fn owned(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn rebase_over_independent_change_is_clean() {
        // My edit (line 1) and onto's edit (line 3) are disjoint.
        let base = doc(&["a", "b", "c", "d"]);
        let mine = doc(&["a", "B", "c", "d"]);
        let onto = doc(&["a", "b", "c", "D"]);

        let r = rebase(&base, &mine, &onto);
        assert!(r.clean);
        assert!(r.conflicts.is_empty());
        // Both my edit and onto's edit are present.
        assert_eq!(r.result, doc(&["a", "B", "c", "D"]));
    }

    #[test]
    fn rebase_over_overlapping_change_carries_a_conflict() {
        // Both sides edit line 1 differently: an overlap.
        let base = doc(&["a", "b", "c"]);
        let mine = doc(&["a", "X", "c"]);
        let onto = doc(&["a", "Y", "c"]);

        let r = rebase(&base, &mine, &onto);
        // Rebase does NOT error; it carries a conflict value.
        assert!(!r.clean);
        assert_eq!(r.conflicts.len(), 1);
        assert_eq!(
            r.conflicts[0],
            Conflict {
                base: owned(&["b"]),
                left: owned(&["X"]),  // my side
                right: owned(&["Y"]), // onto's side
            }
        );
        // Rendered with mine/onto markers, mine on top.
        assert_eq!(
            r.result,
            doc(&["a", REBASE_MINE, "X", REBASE_SEP, "Y", REBASE_ONTO, "c"])
        );
    }

    #[test]
    fn maps_over_conflict_preserves_it() {
        // A prior merge carrying one unresolved conflict.
        let base = doc(&["a", "b", "c"]);
        let mine = doc(&["a", "X", "c"]);
        let onto = doc(&["a", "Y", "c"]);
        let prior = merge3(&base, &mine, &onto);
        assert_eq!(prior.conflicts.len(), 1);

        // A further, independent change: prepend a line, disjoint from the
        // conflict region. Expressed as a patch against the rendered document.
        let with_top = {
            let mut v = vec!["TOP".to_owned()];
            v.extend(prior.merged.lines().iter().cloned());
            Doc::from_lines(v)
        };
        let onto_patch = diff(&prior.merged, &with_top);

        let rebased = rebase_merge(&prior, &onto_patch);
        // The conflict is still present (mapped forward as a value).
        assert_eq!(rebased.conflicts, prior.conflicts);
        // The independent change was applied.
        assert_eq!(
            rebased.merged.lines().first().map(String::as_str),
            Some("TOP")
        );
        // The conflict markers still delimit the same region below TOP.
        assert!(rebased.merged.lines().iter().any(|l| l == CONFLICT_START));
    }

    #[test]
    fn maps_over_conflict_withholds_overlapping_change() {
        // An "independent" change that actually lands inside the conflict region
        // must not be allowed to resolve the conflict: the conflict survives.
        let base = doc(&["a", "b", "c"]);
        let mine = doc(&["a", "X", "c"]);
        let onto = doc(&["a", "Y", "c"]);
        let prior = merge3(&base, &mine, &onto);

        // A patch that would rewrite the "X" line strictly inside the region.
        let inside = Patch::from_hunks(vec![Hunk {
            base_start: 2, // the "X" line within <<<< / ==== markers
            remove: owned(&["X"]),
            insert: owned(&["Z"]),
        }]);
        let rebased = rebase_merge(&prior, &inside);
        // Conflict preserved; the overlapping edit was withheld.
        assert_eq!(rebased.conflicts, prior.conflicts);
        assert!(rebased.merged.lines().iter().any(|l| l == "X"));
        assert!(!rebased.merged.lines().iter().any(|l| l == "Z"));
    }

    #[test]
    fn resolve_collapses_to_chosen_lines() {
        let conflict = Conflict {
            base: owned(&["b"]),
            left: owned(&["X"]),
            right: owned(&["Y"]),
        };
        // Pick my side.
        assert_eq!(resolve(&conflict, &conflict.left), owned(&["X"]));
        // Or a hand-authored reconciliation.
        assert_eq!(resolve(&conflict, &owned(&["Z"])), owned(&["Z"]));
    }

    #[test]
    fn resolve_all_collapses_the_term() {
        let base = doc(&["a", "b", "c"]);
        let mine = doc(&["a", "X", "c"]);
        let onto = doc(&["a", "Y", "c"]);
        let r = rebase(&base, &mine, &onto);
        assert!(!r.clean);

        // Resolve the single conflict to "Z". The term must be gone.
        let resolved = resolve_all(&r.result, &[owned(&["Z"])]);
        assert_eq!(resolved, doc(&["a", "Z", "c"]));
        // No marker lines remain.
        assert!(!resolved
            .lines()
            .iter()
            .any(|l| is_conflict_start(l) || is_conflict_end(l) || l == REBASE_SEP));
    }

    #[test]
    fn resolve_all_can_defer_unresolved_regions() {
        let base = doc(&["a", "b", "c"]);
        let mine = doc(&["a", "X", "c"]);
        let onto = doc(&["a", "Y", "c"]);
        let r = rebase(&base, &mine, &onto);
        // No resolutions supplied: the region is kept intact (deferred).
        let deferred = resolve_all(&r.result, &[]);
        assert_eq!(deferred, r.result);
    }

    #[test]
    fn rebase_stack_all_independent_is_clean() {
        // A stack of three successive versions, each editing a distinct line, and
        // an onto that edits a fourth line — all independent.
        let base = doc(&["a", "b", "c", "d", "e"]);
        let v1 = doc(&["A", "b", "c", "d", "e"]); // edit line 0
        let v2 = doc(&["A", "B", "c", "d", "e"]); // edit line 1
        let v3 = doc(&["A", "B", "C", "d", "e"]); // edit line 2
        let onto = doc(&["a", "b", "c", "d", "E"]); // edit line 4

        let results = rebase_stack(&base, &[v1, v2, v3], &onto);
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.clean));
        // The last step's result carries every edit and onto's edit.
        assert_eq!(results[2].result, doc(&["A", "B", "C", "d", "E"]));
    }

    #[test]
    fn rebase_stack_carries_a_conflicting_step() {
        // The middle step conflicts with onto; earlier and later steps are
        // independent and must still apply — one conflict does not abort the
        // stack. Edits are kept apart by unchanged anchor lines so each lands in
        // its own diff3 region (adjacent opposite-side edits would merge into one
        // region and read as a conflict at the line layer).
        let base = doc(&["a", "b", "c", "d", "e"]);
        let v1 = doc(&["a", "b", "c", "d", "E"]); // step 1: edit line 4 (independent)
        let v2 = doc(&["a", "X", "c", "d", "E"]); // step 2: edit line 1 (conflicts with onto)
        let v3 = doc(&["a", "X", "c", "D", "E"]); // step 3: edit line 3 (independent)
        let onto = doc(&["a", "Y", "c", "d", "e"]); // onto also edits line 1

        let results = rebase_stack(&base, &[v1, v2, v3], &onto);
        assert_eq!(results.len(), 3);
        // Step 1 is clean and lands the line-4 edit on top of onto.
        assert!(results[0].clean);
        // Step 2 conflicts (line 1: X vs Y) and carries the value — it does not
        // abort the stack.
        assert!(!results[1].clean);
        assert_eq!(results[1].conflicts.len(), 1);
        // Step 3 is independent and still applies its line-3 edit despite the
        // unresolved conflict riding along above it in the document.
        assert!(results[2].result.lines().iter().any(|l| l == "D"));
        // The conflict from step 2 is still present in the document at the end of
        // the stack (carried forward as the rendered marker block).
        assert!(results[2].result.lines().iter().any(|l| l == REBASE_MINE));
    }

    #[test]
    fn resolve_then_rebase_equals_rebase_then_resolve() {
        // Bounded confluence (I4) demonstration on a concrete fixture. This is a
        // tested instance, NOT a proof: the design doc holds I4 as an elevation
        // target guarded per-instance (I12), and this exercises one such instance.
        let base = doc(&["a", "b", "c"]);
        let mine = doc(&["a", "X", "c"]);
        let onto = doc(&["a", "Y", "c"]);

        let r = rebase(&base, &mine, &onto);
        assert!(!r.clean);

        // An independent further change: prepend "TOP" (anchored at line 0, so its
        // coordinates are stable across both documents below).
        let onto_patch = Patch::from_hunks(vec![Hunk {
            base_start: 0,
            remove: Vec::new(),
            insert: owned(&["TOP"]),
        }]);
        let chosen = owned(&["Z"]);
        let resolutions = std::slice::from_ref(&chosen);

        // Path A — resolve, then rebase over the independent change.
        let resolved_first = resolve_all(&r.result, resolutions);
        let path_a = apply(&onto_patch, &resolved_first).expect("independent change applies");

        // Path B — rebase (mapping over the conflict), then resolve.
        let rebased_first = rebase_merge(&r.as_merge(), &onto_patch);
        let path_b = resolve_all(&rebased_first.merged, resolutions);

        // Confluence: the two orders reach the same final document.
        assert_eq!(path_a, path_b);
        assert_eq!(path_a, doc(&["TOP", "a", "Z", "c"]));
    }
}
