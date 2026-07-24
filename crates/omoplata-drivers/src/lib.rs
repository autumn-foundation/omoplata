//! omoplata's **Tier-2 structural merge drivers** (design doc §7 crate #5,
//! `omoplata-drivers`).
//!
//! The design doc's merge pipeline (§4) escalates surviving conflicts to a
//! per-language *structural* driver before falling back to a semantic conflict:
//!
//! > *Tier 2 — Structural.* Surviving conflicts go to the per-language
//! > structural driver: parse base and both sides to concrete syntax trees,
//! > match nodes … propose a merged tree. … Kills the false-conflict class:
//! > reformatting, moves, renames, reorderings.
//!
//! > *Tier 3 — Semantic conflict.* What survives Tiers 1–2, or fails dynamic
//! > validation, is presented as a semantic conflict: both sides' definition-level
//! > intent, provenance … and embedding-derived context — not `<<<<<<<` soup.
//!
//! Per §8 scope, v1 implements "Tier-2 structural merge for **Rust only** (one
//! grammar, dogfooded on the Autumn stack), **Mergiraf as the fallback driver**
//! for everything else". Mergiraf is an external binary that is **not** vendored
//! or linked here; when it is present on `PATH` the [`MergirafDriver`] shells out
//! to it for supported non-Rust files, and the built-in [`LineDriver`] is the
//! no-tool fallback when it is absent. See `docs/adr/0004-merge-drivers.md`.
//!
//! # Trust boundary
//!
//! These drivers are **untrusted by design** (design doc §7 crate table; §4
//! principle **P1**, the LCF architecture). A driver is a *proposer*: it emits a
//! candidate merge that the verified kernel admits only after checking tree
//! equality and trivia conservation (**I11**). The invariant the driver layer
//! itself upholds is the honest-degradation rule of **I8**: every result is
//! either a clean merge or a first-class [`Conflict`](omoplata_algebra::Conflict)
//! value — never a silently dropped or silently-picked side. This crate does not
//! host the kernel admission check yet; that wiring is a later milestone.
//!
//! # Drivers
//!
//! * [`RustStructuralDriver`] — Tier-2 structural merge for Rust, at definition
//!   granularity via tree-sitter. Merges cleanly where a line merge conflicts
//!   (e.g. two branches each appending a new item at the same location).
//! * [`MergirafDriver`] — a PATH-detected shell-out to the external
//!   [Mergiraf](https://mergiraf.org) tool, the Tier-2 structural fallback for
//!   45+ non-Rust languages. Selected only when the binary is available.
//! * [`LineDriver`] — the diff3 line fallback used when Mergiraf is absent or
//!   the extension is unsupported, so the crate needs no external tool.
//!
//! [`select_driver`] picks the driver by file extension (and, for non-Rust
//! paths, by whether Mergiraf is available and supports the extension).
//!
//! # Example
//!
//! ```
//! use omoplata_drivers::{select_driver, MergeInput};
//!
//! let base = "fn a() {}\n\nfn b() {}\n";
//! let left = "fn a() {}\n\nfn b() {}\n\nfn c() {}\n";
//! let right = "fn a() {}\n\nfn b() {}\n\nfn d() {}\n";
//! let driver = select_driver("lib.rs");
//! let out = driver
//!     .merge(&MergeInput { base, left, right, path: "lib.rs" })
//!     .expect("merge");
//! assert_eq!(driver.name(), "rust-structural");
//! // Structural merge succeeds where a line merge would conflict.
//! assert!(out.is_clean());
//! assert!(out.merged.contains("fn c()") && out.merged.contains("fn d()"));
//! ```

mod error;
mod line;
mod mergiraf;
pub mod rust;

pub use error::DriverError;
pub use line::LineDriver;
pub use mergiraf::{mergiraf_available, parse_diff3_conflicts, supports_extension, MergirafDriver};
pub use rust::RustStructuralDriver;

/// The three sides of a three-way merge plus the path being merged.
///
/// `base` is the common ancestor; `left` and `right` are the two divergent
/// versions. `path` selects the driver (its extension) and is available to
/// drivers for diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct MergeInput<'a> {
    /// The common ancestor text.
    pub base: &'a str,
    /// The left side's text.
    pub left: &'a str,
    /// The right side's text.
    pub right: &'a str,
    /// The path being merged (drives extension-based selection).
    pub path: &'a str,
}

/// The result of a driver merge.
///
/// `merged` is the reconstructed text, with conflicted regions rendered using
/// `<<<<<<<` / `=======` / `>>>>>>>` markers; `conflicts` is the authoritative
/// list of structured [`Conflict`](omoplata_algebra::Conflict) values (the
/// source of truth, per §5.4). `driver` names the driver that actually produced
/// the output — note the structural driver reports `"line"` here when it falls
/// back on unparseable input.
#[derive(Debug, Clone)]
pub struct DriverOutput {
    /// The reconstructed text (with marker-rendered conflicts, if any).
    pub merged: String,
    /// The conflicts as structured values; empty iff the merge is clean.
    pub conflicts: Vec<omoplata_algebra::Conflict>,
    /// Conflict **values carried forward** from the inputs (§5.4, P3): regions
    /// that were already conflicted before this merge and rode through it
    /// untouched. Distinct from `conflicts`, which this merge produced. Only
    /// the Rust structural driver populates this today.
    pub carried: Vec<omoplata_algebra::Conflict>,
    /// The name of the driver that produced this output.
    pub driver: &'static str,
}

impl DriverOutput {
    /// Whether the merge produced no conflicts and carries none forward.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty() && self.carried.is_empty()
    }

    /// Whether this merge produced no **new** conflicts (conflict values
    /// carried forward from the inputs are allowed — they ride through, per
    /// §5.4, and do not make the merge itself a failure).
    #[must_use]
    pub fn is_mergeable(&self) -> bool {
        self.conflicts.is_empty()
    }
}

/// A Tier-2 merge driver: an untrusted proposer that maps a [`MergeInput`] to a
/// [`DriverOutput`].
pub trait MergeDriver {
    /// The stable driver name (e.g. `"rust-structural"`, `"line"`).
    fn name(&self) -> &'static str;

    /// Merge the three sides of `input`.
    ///
    /// # Errors
    ///
    /// Returns a [`DriverError`] if the driver cannot produce a result. The
    /// built-in drivers do not error in normal operation (the structural driver
    /// falls back to the line driver on unparseable input).
    fn merge(&self, input: &MergeInput) -> Result<DriverOutput, DriverError>;
}

/// Select a driver for `path` by file extension.
///
/// * `.rs` files use the [`RustStructuralDriver`] (Tier-2 structural, the point
///   of M5).
/// * Any other path uses the [`MergirafDriver`] **if** the `mergiraf` binary is
///   available on `PATH` ([`mergiraf_available`]) **and** the extension is one
///   Mergiraf supports ([`supports_extension`]).
/// * Otherwise it falls back to the built-in [`LineDriver`], so selection always
///   returns a working driver even with no external tool present.
///
/// Matching is on the final extension only.
#[must_use]
pub fn select_driver(path: &str) -> Box<dyn MergeDriver> {
    if has_extension(path, "rs") {
        Box::new(RustStructuralDriver::new())
    } else if mergiraf_available() && supports_extension(path) {
        Box::new(MergirafDriver::new())
    } else {
        Box::new(LineDriver::new())
    }
}

/// Whether `path`'s final extension equals `ext` (ASCII, case-sensitive).
fn has_extension(path: &str, ext: &str) -> bool {
    path.rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(_, e)| e == ext)
}

#[cfg(test)]
mod tests;
