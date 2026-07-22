//! The Rust **Tier-2 structural** driver — the point of milestone M5.
//!
//! Design doc §4, Tier 2:
//!
//! > *Tier 2 — Structural.* Surviving conflicts go to the per-language
//! > structural driver: parse base and both sides to concrete syntax trees,
//! > match nodes … propose a merged tree. … Kills the false-conflict class:
//! > reformatting, moves, renames, reorderings.
//!
//! and §8 scope: "Tier-2 structural merge for **Rust only** (one grammar,
//! dogfooded on the Autumn stack)".
//!
//! [`RustStructuralDriver`] merges at **definition granularity** rather than at
//! the line, so two branches that each append a new top-level item at the same
//! textual location merge cleanly where a pure line merge conflicts. It reuses
//! the tree-sitter extraction and tiered matcher from `omoplata-identity`
//! (definition identity, principle **P6**) to pair items across versions, and
//! reuses [`omoplata_algebra::merge3`] to line-merge the interior of a single
//! definition edited on both sides.
//!
//! # Driver-layer trust
//!
//! This driver is **untrusted by design** (design doc §7 crate table, and §4
//! principle **P1**): it is a *proposer*. In the full system its output is a
//! candidate merge that the verified kernel admits only after checking tree
//! equality and trivia conservation (invariant **I11**). This crate does not
//! yet host that kernel check; the driver's own discipline is to degrade to an
//! honest [`Conflict`](omoplata_algebra::Conflict) value rather than guess, and
//! never to drop or silently pick a side of a genuine conflict.
//!
//! # Reassembly order
//!
//! The merged output is assembled deterministically (design doc's canonical-order
//! discipline): **surviving base items in base order, then items added only on
//! the left in left order, then items added only on the right in right order**,
//! each carrying the inter-item text (blank lines, free comments) that preceded
//! it in its source, followed by the base file's trailing text.
//!
//! # Parse fallback
//!
//! tree-sitter is error-tolerant: it recovers from malformed input and still
//! returns a best-effort tree with `ERROR` / `MISSING` nodes. Structurally
//! merging a partially-parsed tree would be unsound, so if any of the three
//! inputs does not parse cleanly (via
//! [`omoplata_identity::parses_cleanly`]) — or a hard grammar/parse error
//! occurs — the driver **falls back to [`LineDriver`](crate::LineDriver)** and
//! returns its output (whose `driver` field is therefore `"line"`, honestly
//! reflecting what ran) rather than erroring or guessing on a broken tree.

use omoplata_algebra::{merge3, Conflict, Doc, CONFLICT_END, CONFLICT_SEP, CONFLICT_START};
use omoplata_identity::{
    extract_definitions, match_definitions, parses_cleanly, Definition, MatchStatus,
};

use crate::{DriverError, DriverOutput, LineDriver, MergeDriver, MergeInput};

/// The Rust Tier-2 structural merge driver.
///
/// Selected by [`select_driver`](crate::select_driver) for `.rs` paths. See the
/// [module docs](crate::rust) for the algorithm, reassembly order, and parse
/// fallback.
#[derive(Debug, Clone, Copy, Default)]
pub struct RustStructuralDriver;

impl RustStructuralDriver {
    /// Create a new structural driver.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl MergeDriver for RustStructuralDriver {
    fn name(&self) -> &'static str {
        "rust-structural"
    }

    /// Structurally merge `base`, `left`, and `right` at definition granularity.
    ///
    /// # Errors
    ///
    /// Does not error on invalid Rust — it falls back to the
    /// [`LineDriver`](crate::LineDriver) (see the [module docs](crate::rust)).
    /// The [`Result`] is retained to satisfy the [`MergeDriver`] contract.
    fn merge(&self, input: &MergeInput) -> Result<DriverOutput, DriverError> {
        // tree-sitter is error-tolerant, so a malformed side still parses to a
        // best-effort tree. Merging partially-parsed trees would be unsound, so
        // if any side does not parse cleanly we fall back to the line driver
        // (documented behavior). A hard grammar/parse error also falls back.
        let clean = [input.base, input.left, input.right]
            .iter()
            .all(|s| parses_cleanly(s).unwrap_or(false));
        if !clean {
            return LineDriver::new().merge(input);
        }

        // Segment all three sides into top-level items + inter-item gaps.
        let (base, left, right) = match (
            Segmentation::of(input.base),
            Segmentation::of(input.left),
            Segmentation::of(input.right),
        ) {
            (Ok(b), Ok(l), Ok(r)) => (b, l, r),
            _ => return LineDriver::new().merge(input),
        };

        Ok(structural_merge(&base, &left, &right))
    }
}

/// One top-level Rust item: its extracted definition metadata plus its exact
/// source text (byte-faithful slice of the original).
struct Item {
    def: Definition,
    text: String,
}

/// A source file segmented into top-level [`Item`]s and the text between them.
///
/// `gaps` has exactly `items.len() + 1` entries: `gaps[i]` is the text that
/// precedes `items[i]` (whitespace, free comments), and `gaps[items.len()]` is
/// the trailing text. Concatenating `gaps[0] item[0] gaps[1] item[1] … gaps[n]`
/// reproduces the original source byte-for-byte, so reassembly is faithful.
struct Segmentation {
    items: Vec<Item>,
    gaps: Vec<String>,
}

impl Segmentation {
    /// Segment `source` into top-level items and gaps.
    ///
    /// Reuses [`extract_definitions`] and keeps only top-level items — a
    /// definition is top-level when it does not start inside the byte range of
    /// an already-taken item (nested definitions start within their parent).
    fn of(source: &str) -> Result<Self, DriverError> {
        let defs = extract_definitions(source)?;

        let mut items: Vec<Item> = Vec::new();
        let mut gaps: Vec<String> = Vec::new();
        let mut covered_end = 0usize;
        let mut cursor = 0usize;

        for def in defs {
            // Skip nested definitions: they begin before the end of the
            // top-level item currently being collected.
            if def.byte_range.start < covered_end {
                continue;
            }
            let (start, end) = (def.byte_range.start, def.byte_range.end);
            // The gap preceding this item is everything since the last cursor.
            let gap = source.get(cursor..start).unwrap_or("").to_owned();
            let text = source.get(start..end).unwrap_or("").to_owned();
            gaps.push(gap);
            items.push(Item { def, text });
            covered_end = end;
            cursor = end;
        }

        // Trailing gap (after the last item, or the whole file if item-free).
        gaps.push(source.get(cursor..).unwrap_or("").to_owned());
        Ok(Self { items, gaps })
    }

    /// The definition metadata of every item, for the identity matcher.
    fn defs(&self) -> Vec<Definition> {
        self.items.iter().map(|it| it.def.clone()).collect()
    }

    /// The source text of item `i`, if present.
    fn text_at(&self, i: usize) -> Option<&str> {
        self.items.get(i).map(|it| it.text.as_str())
    }
}

/// How each side related a base item to one of its own items.
struct Pairing {
    /// `base_to_side[i]` = the side index paired with base item `i`, or `None`
    /// if the base item was deleted on that side.
    base_to_side: Vec<Option<usize>>,
    /// Side item indices that are additions (no base counterpart).
    added: Vec<usize>,
}

/// Pair `base` items against one side using the identity matcher (§5.5), which
/// carries identity across renames and moves, not just exact `(kind, path)`.
fn pair(base: &Segmentation, side: &Segmentation) -> Pairing {
    let base_defs = base.defs();
    let side_defs = side.defs();
    let mut base_to_side = vec![None; base.items.len()];
    let mut added = Vec::new();

    for m in match_definitions(&base_defs, &side_defs) {
        match m.status {
            MatchStatus::Unchanged | MatchStatus::Modified | MatchStatus::Renamed => {
                if let (Some(o), Some(n)) = (m.old, m.new) {
                    if let Some(slot) = base_to_side.get_mut(o) {
                        *slot = Some(n);
                    }
                }
            }
            MatchStatus::Added => {
                if let Some(n) = m.new {
                    added.push(n);
                }
            }
            MatchStatus::Deleted => { /* leaves base_to_side[o] = None */ }
        }
    }
    Pairing {
        base_to_side,
        added,
    }
}

/// The outcome of resolving a single base item across both sides.
enum Resolution {
    /// Keep this text as the item's merged body.
    Keep(String),
    /// The item is deleted on both sides (or deleted one side, untouched other).
    Drop,
    /// A definition-level conflict: rendered text plus the structured values.
    Conflict {
        rendered: String,
        conflicts: Vec<Conflict>,
    },
}

/// Split a text into owned lines (the shape of [`Conflict`]'s fields).
fn lines(text: &str) -> Vec<String> {
    text.split('\n').map(str::to_owned).collect()
}

/// Render a definition-level conflict with the same markers the line driver uses.
fn render_conflict(left: &str, right: &str) -> String {
    format!("{CONFLICT_START}\n{left}\n{CONFLICT_SEP}\n{right}\n{CONFLICT_END}")
}

/// Resolve one base item given its text on each side (`None` = deleted there).
fn resolve_base_item(base_text: &str, left: Option<&str>, right: Option<&str>) -> Resolution {
    match (left, right) {
        (Some(l), Some(r)) => {
            let left_changed = l != base_text;
            let right_changed = r != base_text;
            match (left_changed, right_changed) {
                (false, false) => Resolution::Keep(base_text.to_owned()),
                (true, false) => Resolution::Keep(l.to_owned()),
                (false, true) => Resolution::Keep(r.to_owned()),
                (true, true) => {
                    // Both sides edited this definition: line-merge its interior.
                    let merged = merge3(
                        &Doc::from_str(base_text),
                        &Doc::from_str(l),
                        &Doc::from_str(r),
                    );
                    if merged.is_clean() {
                        Resolution::Keep(merged.merged.to_string())
                    } else {
                        Resolution::Conflict {
                            rendered: merged.merged.to_string(),
                            conflicts: merged.conflicts,
                        }
                    }
                }
            }
        }
        // Deleted on the right.
        (Some(l), None) => {
            if l == base_text {
                Resolution::Drop // deleted one side, untouched the other ⇒ drop
            } else {
                // delete/modify ⇒ conflict.
                Resolution::Conflict {
                    rendered: render_conflict(l, ""),
                    conflicts: vec![Conflict {
                        base: lines(base_text),
                        left: lines(l),
                        right: Vec::new(),
                    }],
                }
            }
        }
        // Deleted on the left.
        (None, Some(r)) => {
            if r == base_text {
                Resolution::Drop
            } else {
                Resolution::Conflict {
                    rendered: render_conflict("", r),
                    conflicts: vec![Conflict {
                        base: lines(base_text),
                        left: Vec::new(),
                        right: lines(r),
                    }],
                }
            }
        }
        // Deleted on both sides.
        (None, None) => Resolution::Drop,
    }
}

/// Run the full definition-granularity structural merge over three segmentations.
fn structural_merge(
    base: &Segmentation,
    left: &Segmentation,
    right: &Segmentation,
) -> DriverOutput {
    let left_pair = pair(base, left);
    let right_pair = pair(base, right);

    // (leading gap, body) segments accumulated in canonical order.
    let mut out: Vec<(String, String)> = Vec::new();
    let mut conflicts: Vec<Conflict> = Vec::new();

    // 1. Surviving base items, in base order.
    for (i, item) in base.items.iter().enumerate() {
        let l = left_pair
            .base_to_side
            .get(i)
            .copied()
            .flatten()
            .and_then(|n| left.text_at(n));
        let r = right_pair
            .base_to_side
            .get(i)
            .copied()
            .flatten()
            .and_then(|n| right.text_at(n));
        let leading = base.gaps.get(i).cloned().unwrap_or_default();
        match resolve_base_item(&item.text, l, r) {
            Resolution::Keep(body) => out.push((leading, body)),
            Resolution::Drop => { /* item removed: its leading gap goes too */ }
            Resolution::Conflict {
                rendered,
                conflicts: mut cs,
            } => {
                conflicts.append(&mut cs);
                out.push((leading, rendered));
            }
        }
    }

    // 2. Items added only on the left, then reconcile with right-added of the
    //    same identity; 3. remaining right-added. `consumed_right` tracks which
    //    right-added indices were matched to a left-added item.
    let mut consumed_right = vec![false; right.items.len()];

    for &lj in &left_pair.added {
        let Some(litem) = left.items.get(lj) else {
            continue;
        };
        let leading = left.gaps.get(lj).cloned().unwrap_or_default();
        // Look for a right-added item with the same (kind, path).
        let counterpart = right_pair.added.iter().copied().find(|&rj| {
            !consumed_right.get(rj).copied().unwrap_or(true)
                && right.items.get(rj).is_some_and(|ri| {
                    ri.def.kind == litem.def.kind && ri.def.path == litem.def.path
                })
        });
        match counterpart {
            Some(rj) => {
                if let Some(slot) = consumed_right.get_mut(rj) {
                    *slot = true;
                }
                let ritem = right.items.get(rj);
                let rtext = ritem.map(|it| it.text.as_str()).unwrap_or("");
                if rtext == litem.text {
                    // Added on both, identical ⇒ include once.
                    out.push((leading, litem.text.clone()));
                } else {
                    // Added on both, same name/kind but differing ⇒ conflict.
                    conflicts.push(Conflict {
                        base: Vec::new(),
                        left: lines(&litem.text),
                        right: lines(rtext),
                    });
                    out.push((leading, render_conflict(&litem.text, rtext)));
                }
            }
            None => out.push((leading, litem.text.clone())),
        }
    }

    // 3. Right-added items with no left counterpart, in right order.
    for &rj in &right_pair.added {
        if consumed_right.get(rj).copied().unwrap_or(false) {
            continue;
        }
        let Some(ritem) = right.items.get(rj) else {
            continue;
        };
        let leading = right.gaps.get(rj).cloned().unwrap_or_default();
        out.push((leading, ritem.text.clone()));
    }

    // Assemble: each segment is its leading gap followed by its body, then the
    // base file's trailing gap closes the document.
    let mut merged = String::new();
    for (gap, body) in &out {
        merged.push_str(gap);
        merged.push_str(body);
    }
    if let Some(trailing) = base.gaps.last() {
        merged.push_str(trailing);
    }

    DriverOutput {
        merged,
        conflicts,
        driver: "rust-structural",
    }
}
