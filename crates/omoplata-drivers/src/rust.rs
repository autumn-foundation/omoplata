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
//! # Container recursion (definition granularity inside `impl`/`mod`/`trait`)
//!
//! When a *container* item (an `impl`, `mod`, or `trait` block) is edited on
//! both sides but its header and footer are byte-identical on all three, the
//! driver recurses: the container's **members** are segmented, matched, and
//! merged with the same definition-granularity algorithm as top-level items.
//! Two sides adding *different* methods to the same `impl` merge cleanly; two
//! sides independently adding a method with the **same name** dedupe when the
//! text is identical and otherwise degrade to an honest, member-scoped
//! conflict — never a silently compiled-in duplicate. A container whose header
//! or footer changed on either side falls back to the interior line merge
//! (previous behavior).
//!
//! # Conflicts as values (§5.4, P3)
//!
//! An input side may itself carry **unresolved conflict values** — regions
//! rendered with `<<<<<<<` / `=======` / `>>>>>>>` markers by an earlier merge.
//! Instead of failing the parse gate and degrading the whole file to a line
//! merge, the driver treats each marker block as an opaque **conflict value**
//! pinned to the definition that contains it:
//!
//! * the input is *sanitized* for parsing (each marker block is replaced by its
//!   left variant), so definition identity still works around the conflict;
//! * a definition carrying a conflict value **rides through** the merge
//!   byte-identically when the other side did not touch that definition — the
//!   carried conflicts are reported in [`DriverOutput::carried`], and the rest
//!   of the file merges structurally as normal ("rebase maps over conflicts;
//!   resolution is a commit that collapses the term");
//! * a side whose text *differs* from a conflict-carrying base is a
//!   **resolution** — it wins, collapsing the term;
//! * when the *other* side also edited a conflict-carrying definition, the
//!   driver nests honestly: a fresh definition-level conflict whose sides are
//!   the full texts (markers included) — never a silent pick.
//!
//! A marker block that is malformed, sits between definitions, or spans a
//! definition boundary bails to the [`LineDriver`](crate::LineDriver) —
//! degraded, but honest about it.
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
//! it in its source, followed by the base file's trailing text. The same order
//! applies to container members during recursion.
//!
//! # Parse fallback
//!
//! tree-sitter is error-tolerant: it recovers from malformed input and still
//! returns a best-effort tree with `ERROR` / `MISSING` nodes. Structurally
//! merging a partially-parsed tree would be unsound, so if any of the three
//! inputs does not parse cleanly (via
//! [`omoplata_identity::parses_cleanly`], after conflict-value sanitization) —
//! or a hard grammar/parse error occurs — the driver **falls back to
//! [`LineDriver`](crate::LineDriver)** and returns its output (whose `driver`
//! field is therefore `"line"`, honestly reflecting what ran) rather than
//! erroring or guessing on a broken tree.

use omoplata_algebra::{merge3, Conflict, Doc, CONFLICT_END, CONFLICT_SEP, CONFLICT_START};
use omoplata_identity::{
    extract_definitions, match_definitions, parses_cleanly, Definition, DefinitionKind, MatchStatus,
};

use crate::{DriverError, DriverOutput, LineDriver, MergeDriver, MergeInput};

/// Maximum container-recursion depth (`mod` in `mod` in `impl` …). Rust
/// nesting in practice is shallow; the guard only bounds pathological input.
const MAX_CONTAINER_DEPTH: usize = 3;

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
        // Sanitize conflict values (marker blocks) out of each side first so
        // that a file *carrying* a conflict still parses; a malformed marker
        // structure bails to the line driver.
        let (Some(base_p), Some(left_p), Some(right_p)) = (
            Prepared::of(input.base),
            Prepared::of(input.left),
            Prepared::of(input.right),
        ) else {
            return LineDriver::new().merge(input);
        };

        // tree-sitter is error-tolerant, so a malformed side still parses to a
        // best-effort tree. Merging partially-parsed trees would be unsound, so
        // if any side does not parse cleanly (after sanitization) we fall back
        // to the line driver (documented behavior). A hard grammar/parse error
        // also falls back.
        let clean = [&base_p, &left_p, &right_p]
            .iter()
            .all(|p| parses_cleanly(&p.sanitized).unwrap_or(false));
        if !clean {
            return LineDriver::new().merge(input);
        }

        // Segment all three sides into top-level items + inter-item gaps, then
        // pin each side's conflict values to the item that contains them. A
        // conflict value outside any item (or spanning items) bails.
        let (base, left, right) = match (base_p.segment(), left_p.segment(), right_p.segment()) {
            (Some(b), Some(l), Some(r)) => (b, l, r),
            _ => return LineDriver::new().merge(input),
        };

        Ok(structural_merge(&base, &left, &right, MAX_CONTAINER_DEPTH))
    }
}

// ---------------------------------------------------------------------------
// Conflict values: marker scanning and sanitization
// ---------------------------------------------------------------------------

/// One `<<<<<<<` / `=======` / `>>>>>>>` marker block found in an input side:
/// its exact original text and its two variants as line vectors.
struct MarkerBlock {
    /// The block's exact original text (markers included, no trailing `\n`).
    original: String,
    /// Lines of the left variant.
    left: Vec<String>,
    /// Lines of the right variant.
    right: Vec<String>,
}

/// The label-free marker prefixes. Rendering uses the algebra's labeled
/// constants ([`CONFLICT_START`] = `<<<<<<< left`, …), but *recognition*
/// accepts any label so values rendered by other tools (e.g. `<<<<<<< HEAD`)
/// still ride through.
const START_PREFIX: &str = "<<<<<<<";
const END_PREFIX: &str = ">>>>>>>";

/// Scan `text` for conflict marker blocks.
///
/// Returns `Some(blocks)` — possibly empty — when the marker structure is
/// well-formed (every start has a separator and an end, no nesting), else
/// `None`. Markers are recognized line-wise: a line *starting with*
/// `<<<<<<<` / `>>>>>>>` (any label), and a line exactly `=======` (the
/// separator carries no label).
fn scan_marker_blocks(text: &str) -> Option<Vec<(usize, usize, MarkerBlock)>> {
    let mut blocks = Vec::new();
    let mut pos = 0usize;
    let mut cur: Option<(usize, Vec<String>, Option<Vec<String>>)> = None;

    for line in text.split_inclusive('\n') {
        let start = pos;
        pos += line.len();
        let stripped = line.strip_suffix('\n').unwrap_or(line);
        // Member-level conflicts rendered inside a container carry the
        // member's leading indentation on the start marker line, so markers
        // are recognized with leading whitespace; the block's range starts at
        // the line start so splicing reproduces the text byte-identically.
        let trimmed = stripped.trim_start();
        if trimmed.starts_with(START_PREFIX) {
            if cur.is_some() {
                return None; // nested start
            }
            cur = Some((start, Vec::new(), None));
        } else if trimmed == CONFLICT_SEP {
            match cur.as_mut() {
                Some((_, _, sep @ None)) => *sep = Some(Vec::new()),
                _ => return None, // separator outside a block, or doubled
            }
        } else if trimmed.starts_with(END_PREFIX) {
            let Some((bstart, left, Some(right))) = cur.take() else {
                return None; // end without start/separator
            };
            // The block ends at the end of this marker line, *excluding* the
            // trailing newline (kept as surrounding text).
            let bend = start + stripped.len();
            let original = text.get(bstart..bend)?.to_owned();
            blocks.push((
                bstart,
                bend,
                MarkerBlock {
                    original,
                    left,
                    right,
                },
            ));
        } else if let Some((_, left, right)) = cur.as_mut() {
            match right {
                Some(r) => r.push(stripped.to_owned()),
                None => left.push(stripped.to_owned()),
            }
        }
    }
    if cur.is_some() {
        return None; // unterminated block
    }
    Some(blocks)
}

/// An input side prepared for structural merging: its sanitized text (each
/// marker block replaced by its left variant, so the side parses) plus the
/// conflict values found, located by their byte range **in the sanitized
/// text**.
struct Prepared {
    sanitized: String,
    /// `(sanitized_range, block)` per conflict value, in text order.
    values: Vec<(std::ops::Range<usize>, MarkerBlock)>,
}

impl Prepared {
    /// Scan and sanitize `text`. `None` iff the marker structure is malformed.
    fn of(text: &str) -> Option<Self> {
        let blocks = scan_marker_blocks(text)?;
        if blocks.is_empty() {
            return Some(Self {
                sanitized: text.to_owned(),
                values: Vec::new(),
            });
        }
        let mut sanitized = String::with_capacity(text.len());
        let mut values = Vec::new();
        let mut cursor = 0usize;
        for (bstart, bend, block) in blocks {
            sanitized.push_str(text.get(cursor..bstart)?);
            let replacement = block.left.join("\n");
            let rstart = sanitized.len();
            sanitized.push_str(&replacement);
            values.push((rstart..sanitized.len(), block));
            cursor = bend;
        }
        sanitized.push_str(text.get(cursor..)?);
        Some(Self { sanitized, values })
    }

    /// Segment the sanitized text and pin each conflict value to the item that
    /// contains it. `None` when segmentation fails or a value falls outside
    /// every item (gap) or spans an item boundary.
    fn segment(&self) -> Option<Segmentation> {
        let mut seg = Segmentation::of(&self.sanitized).ok()?;
        // Collect (item index, relative range, block) pins first.
        let mut pins: Vec<(usize, std::ops::Range<usize>, &MarkerBlock)> = Vec::new();
        for (range, block) in &self.values {
            let idx = seg.items.iter().position(|it| {
                it.byte_range.start <= range.start && range.end <= it.byte_range.end
            })?;
            let item_start = seg.items[idx].byte_range.start;
            pins.push((idx, range.start - item_start..range.end - item_start, block));
        }
        // Splice originals back per item, last-to-first so offsets stay valid,
        // and record the carried conflict values.
        for (idx, rel, block) in pins.into_iter().rev() {
            let item = seg.items.get_mut(idx)?;
            item.original_text.replace_range(rel, &block.original);
            item.carried.insert(
                0,
                Conflict {
                    base: Vec::new(),
                    left: block.left.clone(),
                    right: block.right.clone(),
                },
            );
        }
        Some(seg)
    }
}

// ---------------------------------------------------------------------------
// Segmentation
// ---------------------------------------------------------------------------

/// One Rust item at the current granularity level: its extracted definition
/// metadata plus its exact source text (byte-faithful slice of the original).
struct Item {
    def: Definition,
    /// The item's sanitized text (conflict values replaced by left variants).
    text: String,
    /// The item's true text, conflict markers included. Equal to `text` for a
    /// conflict-free item.
    original_text: String,
    /// Conflict values pinned to this item (empty for a conflict-free item).
    carried: Vec<Conflict>,
    /// The item's byte range in the (sanitized) source it was cut from.
    byte_range: std::ops::Range<usize>,
}

impl Item {
    fn conflict_free(def: Definition, text: String, byte_range: std::ops::Range<usize>) -> Self {
        Self {
            original_text: text.clone(),
            text,
            def,
            carried: Vec::new(),
            byte_range,
        }
    }
}

/// A source file segmented into [`Item`]s and the text between them.
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
            items.push(Item::conflict_free(def, text, start..end));
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

    /// The item at index `i`, if present.
    fn item_at(&self, i: usize) -> Option<&Item> {
        self.items.get(i)
    }
}

// ---------------------------------------------------------------------------
// Pairing
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Per-item resolution
// ---------------------------------------------------------------------------

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
    /// A conflict value riding through unchanged (§5.4): the carrier side's
    /// text, markers included, plus the values it carries.
    Carry {
        rendered: String,
        carried: Vec<Conflict>,
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

/// Resolve one base item given its counterpart on each side (`None` = deleted
/// there). Comparison is on **original** (marker-inclusive) text, so a side
/// that collapses a carried conflict registers as a change — a resolution.
fn resolve_base_item(
    base: &Item,
    left: Option<&Item>,
    right: Option<&Item>,
    depth: usize,
) -> Resolution {
    match (left, right) {
        (Some(l), Some(r)) => {
            let left_changed = l.original_text != base.original_text;
            let right_changed = r.original_text != base.original_text;
            match (left_changed, right_changed) {
                (false, false) => {
                    if base.carried.is_empty() {
                        Resolution::Keep(base.original_text.clone())
                    } else {
                        // Nobody resolved the base's conflict: it rides through.
                        Resolution::Carry {
                            rendered: base.original_text.clone(),
                            carried: base.carried.clone(),
                        }
                    }
                }
                (true, false) => keep_or_carry(l),
                (false, true) => keep_or_carry(r),
                (true, true) => {
                    if !l.carried.is_empty() || !r.carried.is_empty() {
                        // Both sides touched a definition and at least one of
                        // them still carries an unresolved conflict: nest
                        // honestly — full texts, markers included.
                        return Resolution::Conflict {
                            rendered: render_conflict(&l.original_text, &r.original_text),
                            conflicts: vec![Conflict {
                                base: lines(&base.original_text),
                                left: lines(&l.original_text),
                                right: lines(&r.original_text),
                            }],
                        };
                    }
                    // Both sides edited this definition. If it is a container
                    // (impl/mod/trait) with an unchanged frame, merge its
                    // members at definition granularity; else line-merge the
                    // interior.
                    if let Some(res) = try_container_merge(base, l, r, depth) {
                        return res;
                    }
                    let merged = merge3(
                        &Doc::from_str(&base.text),
                        &Doc::from_str(&l.text),
                        &Doc::from_str(&r.text),
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
            if l.original_text == base.original_text {
                Resolution::Drop // deleted one side, untouched the other ⇒ drop
            } else {
                // delete/modify ⇒ conflict.
                Resolution::Conflict {
                    rendered: render_conflict(&l.original_text, ""),
                    conflicts: vec![Conflict {
                        base: lines(&base.original_text),
                        left: lines(&l.original_text),
                        right: Vec::new(),
                    }],
                }
            }
        }
        // Deleted on the left.
        (None, Some(r)) => {
            if r.original_text == base.original_text {
                Resolution::Drop
            } else {
                Resolution::Conflict {
                    rendered: render_conflict("", &r.original_text),
                    conflicts: vec![Conflict {
                        base: lines(&base.original_text),
                        left: Vec::new(),
                        right: lines(&r.original_text),
                    }],
                }
            }
        }
        // Deleted on both sides.
        (None, None) => Resolution::Drop,
    }
}

/// Keep a changed side's text, as a ride-through when it carries conflicts.
fn keep_or_carry(side: &Item) -> Resolution {
    if side.carried.is_empty() {
        Resolution::Keep(side.original_text.clone())
    } else {
        Resolution::Carry {
            rendered: side.original_text.clone(),
            carried: side.carried.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Container recursion
// ---------------------------------------------------------------------------

/// Whether items of this kind get member-granularity recursion.
fn recursable(kind: DefinitionKind) -> bool {
    matches!(
        kind,
        DefinitionKind::Impl | DefinitionKind::Module | DefinitionKind::Trait
    )
}

/// A container item split into `header { members } footer`.
struct ContainerParts {
    header: String,
    members: Segmentation,
    footer: String,
}

/// Split a container item's text into header (through the opening `{`),
/// member segmentation, and footer (from the closing `}`). `None` when the
/// text does not have the expected single-container shape.
fn split_container(text: &str) -> Option<ContainerParts> {
    let defs = extract_definitions(text).ok()?;
    // The first definition must be the container itself, spanning the text.
    let container = defs.first()?;
    if container.byte_range.start != 0 || container.byte_range.end != text.len() {
        return None;
    }
    let header_end = text.find('{')? + 1;
    let footer_start = text.rfind('}')?;
    if footer_start < header_end {
        return None;
    }

    let mut items: Vec<Item> = Vec::new();
    let mut gaps: Vec<String> = Vec::new();
    let mut covered_end = header_end;
    let mut cursor = header_end;
    for def in defs.iter().skip(1) {
        if def.byte_range.start < covered_end {
            continue; // doubly-nested definition; belongs to a member
        }
        if def.byte_range.start < header_end || def.byte_range.end > footer_start {
            return None; // member escapes the braces — unexpected shape
        }
        let (start, end) = (def.byte_range.start, def.byte_range.end);
        gaps.push(text.get(cursor..start)?.to_owned());
        items.push(Item::conflict_free(
            def.clone(),
            text.get(start..end)?.to_owned(),
            start..end,
        ));
        covered_end = end;
        cursor = end;
    }
    gaps.push(text.get(cursor..footer_start)?.to_owned());

    Some(ContainerParts {
        header: text.get(..header_end)?.to_owned(),
        members: Segmentation { items, gaps },
        footer: text.get(footer_start..)?.to_owned(),
    })
}

/// Attempt member-granularity merge of a container edited on both sides.
///
/// Applies only when all three items are the same recursable kind and their
/// headers and footers are byte-identical; otherwise `None`, and the caller
/// falls back to the interior line merge.
fn try_container_merge(base: &Item, l: &Item, r: &Item, depth: usize) -> Option<Resolution> {
    if depth == 0 {
        return None;
    }
    if !(recursable(base.def.kind) && l.def.kind == base.def.kind && r.def.kind == base.def.kind) {
        return None;
    }
    let bp = split_container(&base.text)?;
    let lp = split_container(&l.text)?;
    let rp = split_container(&r.text)?;
    if !(bp.header == lp.header && bp.header == rp.header) {
        return None;
    }
    if !(bp.footer == lp.footer && bp.footer == rp.footer) {
        return None;
    }

    let out = structural_merge(&bp.members, &lp.members, &rp.members, depth - 1);
    let rendered = format!("{}{}{}", bp.header, out.merged, bp.footer);
    if out.conflicts.is_empty() && out.carried.is_empty() {
        Some(Resolution::Keep(rendered))
    } else if out.conflicts.is_empty() {
        Some(Resolution::Carry {
            rendered,
            carried: out.carried,
        })
    } else {
        // Fresh member-level conflicts (carried ones, if any, are folded into
        // the conflict list so no value is lost at this boundary).
        let mut conflicts = out.conflicts;
        conflicts.extend(out.carried);
        Some(Resolution::Conflict {
            rendered,
            conflicts,
        })
    }
}

// ---------------------------------------------------------------------------
// The merge
// ---------------------------------------------------------------------------

/// Run the full definition-granularity structural merge over three segmentations.
fn structural_merge(
    base: &Segmentation,
    left: &Segmentation,
    right: &Segmentation,
    depth: usize,
) -> DriverOutput {
    let left_pair = pair(base, left);
    let right_pair = pair(base, right);

    // (leading gap, body) segments accumulated in canonical order.
    let mut out: Vec<(String, String)> = Vec::new();
    let mut conflicts: Vec<Conflict> = Vec::new();
    let mut carried: Vec<Conflict> = Vec::new();

    // 1. Surviving base items, in base order.
    for (i, item) in base.items.iter().enumerate() {
        let l = left_pair
            .base_to_side
            .get(i)
            .copied()
            .flatten()
            .and_then(|n| left.item_at(n));
        let r = right_pair
            .base_to_side
            .get(i)
            .copied()
            .flatten()
            .and_then(|n| right.item_at(n));
        let leading = base.gaps.get(i).cloned().unwrap_or_default();
        match resolve_base_item(item, l, r, depth) {
            Resolution::Keep(body) => out.push((leading, body)),
            Resolution::Drop => { /* item removed: its leading gap goes too */ }
            Resolution::Conflict {
                rendered,
                conflicts: mut cs,
            } => {
                conflicts.append(&mut cs);
                out.push((leading, rendered));
            }
            Resolution::Carry {
                rendered,
                carried: mut cv,
            } => {
                carried.append(&mut cv);
                out.push((leading, rendered));
            }
        }
    }

    // 2. Items added only on the left, then reconcile with right-added of the
    //    same identity; 3. remaining right-added. `consumed_right` tracks which
    //    right-added indices were matched to a left-added item.
    let mut consumed_right = vec![false; right.items.len()];

    for &lj in &left_pair.added {
        let Some(litem) = left.item_at(lj) else {
            continue;
        };
        let leading = left.gaps.get(lj).cloned().unwrap_or_default();
        // Look for a right-added item with the same (kind, path).
        let counterpart = right_pair.added.iter().copied().find(|&rj| {
            !consumed_right.get(rj).copied().unwrap_or(true)
                && right.item_at(rj).is_some_and(|ri| {
                    ri.def.kind == litem.def.kind && ri.def.path == litem.def.path
                })
        });
        match counterpart {
            Some(rj) => {
                if let Some(slot) = consumed_right.get_mut(rj) {
                    *slot = true;
                }
                let ritem = right.item_at(rj);
                let rtext = ritem.map(|it| it.original_text.as_str()).unwrap_or("");
                if rtext == litem.original_text {
                    // Added on both, identical ⇒ include once.
                    push_added(&mut out, &mut conflicts, &mut carried, leading, litem);
                } else {
                    // Added on both, same name/kind but differing ⇒ conflict.
                    conflicts.push(Conflict {
                        base: Vec::new(),
                        left: lines(&litem.original_text),
                        right: lines(rtext),
                    });
                    out.push((leading, render_conflict(&litem.original_text, rtext)));
                }
            }
            None => push_added(&mut out, &mut conflicts, &mut carried, leading, litem),
        }
    }

    // 3. Right-added items with no left counterpart, in right order.
    for &rj in &right_pair.added {
        if consumed_right.get(rj).copied().unwrap_or(false) {
            continue;
        }
        let Some(ritem) = right.item_at(rj) else {
            continue;
        };
        let leading = right.gaps.get(rj).cloned().unwrap_or_default();
        push_added(&mut out, &mut conflicts, &mut carried, leading, ritem);
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
        carried,
        driver: "rust-structural",
    }
}

/// Emit an added item, propagating any conflict values it carries.
fn push_added(
    out: &mut Vec<(String, String)>,
    _conflicts: &mut [Conflict],
    carried: &mut Vec<Conflict>,
    leading: String,
    item: &Item,
) {
    if !item.carried.is_empty() {
        carried.extend(item.carried.iter().cloned());
    }
    out.push((leading, item.original_text.clone()));
}

// ---------------------------------------------------------------------------
// Conflict-value queries
// ---------------------------------------------------------------------------

/// One conflict value found in a source file: where it is and what contains it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictValue {
    /// Qualified path of the definition containing the value, if any
    /// (`None` when the value sits between definitions).
    pub definition: Option<String>,
    /// 1-based line of the `<<<<<<<` marker in the original text.
    pub line: usize,
    /// The left variant's lines.
    pub left: Vec<String>,
    /// The right variant's lines.
    pub right: Vec<String>,
}

/// List the conflict values carried by `source`, each pinned to the definition
/// containing it (§5.4: conflicts are first-class, queryable values).
///
/// Returns an empty list for a conflict-free file, and `None` when the marker
/// structure is malformed.
#[must_use]
pub fn conflict_values(source: &str) -> Option<Vec<ConflictValue>> {
    let prepared = Prepared::of(source)?;
    if prepared.values.is_empty() {
        return Some(Vec::new());
    }
    let defs = extract_definitions(&prepared.sanitized).ok()?;

    // Recover each value's line in the ORIGINAL text by re-scanning blocks.
    let blocks = scan_marker_blocks(source)?;

    let mut out = Vec::new();
    for ((range, _), (bstart, _, block)) in prepared.values.iter().zip(blocks) {
        // The deepest definition containing the sanitized range.
        let definition = defs
            .iter()
            .filter(|d| d.byte_range.start <= range.start && range.end <= d.byte_range.end)
            .max_by_key(|d| d.byte_range.start)
            .map(|d| d.path.clone());
        let line = source[..bstart].matches('\n').count() + 1;
        out.push(ConflictValue {
            definition,
            line,
            left: block.left,
            right: block.right,
        });
    }
    Some(out)
}
