//! Three-way merge with conflicts-as-values (design doc §5.4).
//!
//! Design doc §5.4:
//!
//! > A conflict is a stored term: `Conflict{base, sides: [tree], provenance}`
//! > embedded in a commit's tree. Rebase maps over conflicts; resolution is a
//! > commit that collapses the term.
//!
//! [`merge3`] runs a diff3-style algorithm over the two patches
//! `diff(base, left)` and `diff(base, right)`. Regions changed by only one side
//! apply cleanly; regions changed identically by both apply once; regions where
//! both sides change the same base span differently become a [`Conflict`]
//! **value** — a stored term, not text markers. The reconstructed [`Doc`]
//! includes a deterministic marker rendering for conflicted regions, but the
//! authoritative data is always the [`Merge::conflicts`] vector.

use crate::doc::Doc;
use crate::patch::lcs_matches;

/// Marker beginning a conflicted region in a rendered [`Merge::merged`] doc.
pub const CONFLICT_START: &str = "<<<<<<< left";
/// Marker separating the two sides of a conflicted region.
pub const CONFLICT_SEP: &str = "=======";
/// Marker ending a conflicted region.
pub const CONFLICT_END: &str = ">>>>>>> right";

/// A conflict stored as a value (design doc §5.4's `Conflict{base, sides, ...}`).
///
/// It records the common `base` span and the two divergent replacements. This
/// is the source of truth for a conflict; any `<<<<<<<`-style rendering is a
/// human view derived from it, never the other way round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// The base lines both sides started from in this region.
    pub base: Vec<String>,
    /// The left side's replacement for `base`.
    pub left: Vec<String>,
    /// The right side's replacement for `base`.
    pub right: Vec<String>,
}

/// The result of a three-way merge: a reconstructed document plus the list of
/// conflicts encountered.
///
/// `merged` is a best-effort reconstruction. For clean regions it is the merged
/// content; for conflicted regions it contains a deterministic marker rendering
/// (see [`CONFLICT_START`] and friends). A merge is *clean* exactly when
/// [`conflicts`](Merge::conflicts) is empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merge {
    /// The reconstructed document (with marker-rendered conflicts, if any).
    pub merged: Doc,
    /// The conflicts as stored terms; empty iff the merge is clean.
    pub conflicts: Vec<Conflict>,
}

impl Merge {
    /// Whether the merge produced no conflicts.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }
}

/// Three-way merge of `left` and `right` against their common `base`.
///
/// PROOF OBLIGATION (I8 — kernel admission, "no silent wrong answers"): every
/// region resolves to one of exactly three admissible outcomes — a clean
/// one-sided change, an identical two-sided change applied once, or a stored
/// [`Conflict`] value. A span changed differently by both sides is *never*
/// silently resolved to one side; it always becomes a conflict. Guarded by the
/// `merge_*` property tests.
///
/// PROOF OBLIGATION (symmetry, cf. I2): the presence of conflicts is invariant
/// under swapping `left` and `right` — region classification depends
/// symmetrically on the two sides. Guarded by `conflict_presence_is_symmetric`.
///
/// Take-one-side identities (`merge3(base, a, base)` is clean with
/// `merged == a`; `merge3(base, base, b)` is clean with `merged == b`;
/// `merge3(base, a, a)` is clean with `merged == a`) are guarded by
/// `merge_takes_the_changed_side` and `merge_identical_edits`.
#[must_use]
pub fn merge3(base: &Doc, left: &Doc, right: &Doc) -> Merge {
    let b = base.lines();
    let l = left.lines();
    let r = right.lines();

    // Alignment of base against each side (LCS). `la[o] = Some(a)` means base
    // line `o` is unchanged in left, matched to left line `a`; likewise `lb`.
    let mut la: Vec<Option<usize>> = vec![None; b.len()];
    for (o, a) in lcs_matches(b, l) {
        la[o] = Some(a);
    }
    let mut lb: Vec<Option<usize>> = vec![None; b.len()];
    for (o, bx) in lcs_matches(b, r) {
        lb[o] = Some(bx);
    }

    let mut merged: Vec<String> = Vec::new();
    let mut conflicts: Vec<Conflict> = Vec::new();
    let (mut i, mut a, mut bx) = (0usize, 0usize, 0usize);

    loop {
        // Emit a stable run: base lines matched, at the current cursors, in both
        // sides simultaneously. These are the sync anchors between regions.
        while i < b.len() && la[i] == Some(a) && lb[i] == Some(bx) {
            merged.push(b[i].clone());
            i += 1;
            a += 1;
            bx += 1;
        }
        if i >= b.len() {
            // Trailing change region: whatever remains of each side after the
            // last common base line.
            emit_region(&[], &l[a..], &r[bx..], &mut merged, &mut conflicts);
            break;
        }

        // Find the next mutual anchor: the smallest base index >= i matched in
        // both sides. The region spans up to (but not including) it.
        let mut j = i;
        while j < b.len() && !(la[j].is_some() && lb[j].is_some()) {
            j += 1;
        }
        let (a2, b2) = if j < b.len() {
            // Both are Some here by the loop condition.
            (la[j].unwrap_or(a), lb[j].unwrap_or(bx))
        } else {
            (l.len(), r.len())
        };

        emit_region(&b[i..j], &l[a..a2], &r[bx..b2], &mut merged, &mut conflicts);
        i = j;
        a = a2;
        bx = b2;
    }

    Merge {
        merged: Doc::from_lines(merged),
        conflicts,
    }
}

/// Classify and emit one change region.
///
/// `o`, `lft`, `rgt` are the base / left / right slices of the region. The
/// classification is symmetric in `lft` and `rgt`:
///
/// * left unchanged (`lft == o`) ⇒ take right;
/// * right unchanged (`rgt == o`) ⇒ take left;
/// * both changed identically (`lft == rgt`) ⇒ take once;
/// * otherwise ⇒ a [`Conflict`] value, rendered with markers into `merged`.
fn emit_region(
    o: &[String],
    lft: &[String],
    rgt: &[String],
    merged: &mut Vec<String>,
    conflicts: &mut Vec<Conflict>,
) {
    if lft == o {
        // Left unchanged ⇒ take right.
        merged.extend_from_slice(rgt);
    } else if rgt == o || lft == rgt {
        // Right unchanged ⇒ take left; or both changed identically ⇒ take once.
        merged.extend_from_slice(lft);
    } else {
        conflicts.push(Conflict {
            base: o.to_vec(),
            left: lft.to_vec(),
            right: rgt.to_vec(),
        });
        merged.push(CONFLICT_START.to_owned());
        merged.extend_from_slice(lft);
        merged.push(CONFLICT_SEP.to_owned());
        merged.extend_from_slice(rgt);
        merged.push(CONFLICT_END.to_owned());
    }
}
