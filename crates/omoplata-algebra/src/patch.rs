//! Diff, patches, and application — the canonical-diff and `apply` half of the
//! patch algebra (design doc §5.2).
//!
//! A [`Patch`] is *only ever* produced by [`diff`]; per §5.2, "since patches
//! exist only as `diff` outputs, determinism yields canonicity by fiat (one
//! patch per pair, cache keys stable)". [`diff`] is a pure function of its two
//! arguments with a fixed tie-break, so identical inputs yield bit-identical
//! patches — the design doc's invariant **I1a (diff determinism)**. [`apply`]
//! realises the round-trip **I1b (diff faithfulness)**:
//! `apply(&diff(a, b), a) == Ok(b)`.

use std::ops::Range;

use crate::doc::Doc;

/// One contiguous edit against the base document.
///
/// A hunk removes `remove.len()` lines starting at base line `base_start` and
/// inserts `insert` in their place. Its **support** (§5.2) is the half-open
/// base interval `[base_start, base_start + remove.len())` — the base lines it
/// touches. A pure insertion has an empty `remove` and therefore an empty
/// support interval anchored at `base_start`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hunk {
    /// Index of the first base line this hunk touches.
    pub base_start: usize,
    /// The base lines this hunk expects to remove (the context that must match
    /// on apply).
    pub remove: Vec<String>,
    /// The lines inserted in place of `remove`.
    pub insert: Vec<String>,
}

impl Hunk {
    /// The half-open base interval this hunk touches: its support.
    #[must_use]
    pub fn support(&self) -> Range<usize> {
        self.base_start..self.base_start + self.remove.len()
    }

    /// The net change in line count this hunk applies: inserted minus removed.
    /// Positive lengthens the document, negative shortens it.
    #[must_use]
    fn delta(&self) -> isize {
        self.insert.len() as isize - self.remove.len() as isize
    }
}

/// An ordered, non-overlapping list of [`Hunk`]s against a common base.
///
/// The invariant maintained by [`diff`] and relied on everywhere else: hunks
/// are sorted by `base_start` and their support intervals do not overlap.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Patch {
    hunks: Vec<Hunk>,
}

impl Patch {
    /// Build a patch from raw hunks. Intended for internal construction and
    /// rebasing; callers normally obtain patches from [`diff`].
    #[must_use]
    pub fn from_hunks(hunks: Vec<Hunk>) -> Self {
        Self { hunks }
    }

    /// The patch's hunks, in `base_start` order.
    #[must_use]
    pub fn hunks(&self) -> &[Hunk] {
        &self.hunks
    }

    /// Whether the patch changes nothing (the diff of a document with itself).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty()
    }

    /// The **support set** of the patch (§5.2): the base line intervals it
    /// touches, one per hunk, in order. Tier-0 disjoint-support commutation
    /// (§4) is decided entirely from these intervals.
    #[must_use]
    pub fn support(&self) -> Vec<Range<usize>> {
        self.hunks.iter().map(Hunk::support).collect()
    }

    /// The cumulative net line-delta contributed by every hunk that ends at or
    /// before base position `pos`. Used to rebase a patch's coordinates past
    /// another patch that has already been applied (see
    /// [`commute`](crate::commute::commute)).
    #[must_use]
    pub(crate) fn delta_before(&self, pos: usize) -> isize {
        self.hunks
            .iter()
            .filter(|h| h.base_start + h.remove.len() <= pos)
            .map(Hunk::delta)
            .sum()
    }
}

/// Errors returned by [`apply`] when a patch does not fit its base.
///
/// This is the executable-check side of the design doc's soundness story: a
/// patch is never applied blindly. A hunk whose `remove` context does not match
/// the base, or that runs off the end of the base, is refused rather than
/// silently misapplied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    /// A hunk's support interval extends past the end of the base document.
    #[error(
        "hunk at base line {base_start} (len {remove_len}) runs past end of base (len {base_len})"
    )]
    OutOfRange {
        /// The offending hunk's `base_start`.
        base_start: usize,
        /// The offending hunk's `remove` length.
        remove_len: usize,
        /// The length of the base document.
        base_len: usize,
    },
    /// A hunk's `remove` lines do not match the base slice it targets (a failed
    /// context check).
    #[error(
        "context mismatch at base line {base_start}: hunk's removed lines do not match the base"
    )]
    ContextMismatch {
        /// The `base_start` of the hunk whose context did not match.
        base_start: usize,
    },
    /// Two hunks' support intervals overlap, so the patch is ill-formed.
    #[error("overlapping hunks: hunk at base line {base_start} starts before the previous hunk ends at {prev_end}")]
    OverlappingHunks {
        /// The `base_start` of the hunk that starts too early.
        base_start: usize,
        /// The base position where the previous hunk's support ends.
        prev_end: usize,
    },
}

/// Compute the deterministic line diff turning `base` into `target`.
///
/// PROOF OBLIGATION (I1a — diff determinism): `diff(a, b)` is a pure function
/// with total tie-breaking; identical inputs yield bit-identical patches.
/// Guaranteed here by [`lcs_matches`]'s fixed tie-break (prefer advancing the
/// base cursor on ties), so there is exactly one patch per `(base, target)`
/// pair. Guarded by the `diff_is_deterministic` property test.
///
/// PROOF OBLIGATION (I1b — diff faithfulness): `apply(&diff(a, b), a) == Ok(b)`
/// exactly. The round-trip theorem. Guarded by the `roundtrip` property test.
///
/// The algorithm is a classic LCS (longest-common-subsequence) line diff:
/// matched lines are left untouched, and the gaps between consecutive matches
/// become [`Hunk`]s. The resulting patch's hunks are sorted by `base_start` and
/// have non-overlapping support.
#[must_use]
pub fn diff(base: &Doc, target: &Doc) -> Patch {
    let b = base.lines();
    let t = target.lines();
    let matches = lcs_matches(b, t);

    let mut hunks = Vec::new();
    let (mut bi, mut ti) = (0usize, 0usize);
    // A sentinel match at (len, len) flushes any trailing edit.
    for (mb, mt) in matches
        .into_iter()
        .chain(std::iter::once((b.len(), t.len())))
    {
        if bi < mb || ti < mt {
            hunks.push(Hunk {
                base_start: bi,
                remove: b[bi..mb].to_vec(),
                insert: t[ti..mt].to_vec(),
            });
        }
        bi = mb + 1;
        ti = mt + 1;
    }
    Patch { hunks }
}

/// Apply `patch` to `base`, returning the reconstructed document.
///
/// Each hunk's `remove` lines are verified against the base slice they target
/// (the context check) before anything is emitted; a mismatch or out-of-range
/// hunk yields an [`ApplyError`] and no document. This is the line-layer
/// analogue of the kernel's executable equality check (design doc §5.2, I5/I8):
/// a patch is admitted against the actual base, never assumed to fit.
///
/// # Errors
/// Returns [`ApplyError::OutOfRange`] if a hunk reaches past the base,
/// [`ApplyError::ContextMismatch`] if a hunk's removed lines do not match the
/// base, or [`ApplyError::OverlappingHunks`] if the patch is ill-formed.
pub fn apply(patch: &Patch, base: &Doc) -> Result<Doc, ApplyError> {
    let b = base.lines();
    let mut out: Vec<String> = Vec::new();
    let mut pos = 0usize;

    for hunk in patch.hunks() {
        if hunk.base_start < pos {
            return Err(ApplyError::OverlappingHunks {
                base_start: hunk.base_start,
                prev_end: pos,
            });
        }
        let end = hunk.base_start + hunk.remove.len();
        if end > b.len() {
            return Err(ApplyError::OutOfRange {
                base_start: hunk.base_start,
                remove_len: hunk.remove.len(),
                base_len: b.len(),
            });
        }
        // Copy the untouched base lines before this hunk.
        out.extend_from_slice(&b[pos..hunk.base_start]);
        // Context check: the base slice must equal what the hunk expects.
        if b[hunk.base_start..end] != hunk.remove[..] {
            return Err(ApplyError::ContextMismatch {
                base_start: hunk.base_start,
            });
        }
        out.extend_from_slice(&hunk.insert);
        pos = end;
    }
    out.extend_from_slice(&b[pos..]);
    Ok(Doc::from_lines(out))
}

/// The longest common subsequence of `a` and `b`, as increasing index pairs.
///
/// Deterministic tie-break: on equal LCS lengths, advance the `a` cursor first.
/// This fixed choice is what makes [`diff`] canonical (I1a) — there is exactly
/// one alignment per input pair, hence one patch.
pub(crate) fn lcs_matches(a: &[String], b: &[String]) -> Vec<(usize, usize)> {
    let (m, n) = (a.len(), b.len());
    // dp[i][j] = length of the LCS of a[i..] and b[j..].
    let mut dp = vec![vec![0u32; n + 1]; m + 1];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let mut res = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < m && j < n {
        if a[i] == b[j] {
            res.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    res
}
