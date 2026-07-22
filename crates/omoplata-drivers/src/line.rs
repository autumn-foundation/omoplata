//! The line-oriented fallback driver — the honest stand-in for **Mergiraf**.
//!
//! The design doc names Mergiraf as the Tier-2 *fallback driver* for every
//! language other than Rust (§4 architecture diagram; §8 scope: "Mergiraf as the
//! fallback driver for everything else"). Mergiraf is an external binary that is
//! not vendored into this environment, so this crate ships a built-in
//! diff3-style driver in its place: [`LineDriver`] runs
//! [`omoplata_algebra::merge3`] over the three texts and maps its result into a
//! [`DriverOutput`](crate::DriverOutput). See
//! `docs/adr/0004-merge-drivers.md` for the substitution rationale.

use omoplata_algebra::{merge3, Doc};

use crate::{DriverError, DriverOutput, MergeDriver, MergeInput};

/// A three-way line merge — the Tier-2 fallback driver.
///
/// Wraps the verified line/opaque merge of [`omoplata_algebra::merge3`]:
/// conflicted regions are rendered into `merged` with `<<<<<<<` / `=======` /
/// `>>>>>>>` markers, and the structured [`Conflict`](omoplata_algebra::Conflict)
/// values are carried through verbatim as the source of truth. This is the
/// driver selected by [`select_driver`](crate::select_driver) for every path
/// that is not a `.rs` file.
#[derive(Debug, Clone, Copy, Default)]
pub struct LineDriver;

impl LineDriver {
    /// Create a new line driver.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl MergeDriver for LineDriver {
    fn name(&self) -> &'static str {
        "line"
    }

    /// Line-merge `base`, `left`, and `right`.
    ///
    /// # Errors
    ///
    /// Never fails: the signature returns [`Result`] only to share the
    /// [`MergeDriver`] contract with fallible drivers.
    fn merge(&self, input: &MergeInput) -> Result<DriverOutput, DriverError> {
        let merge = merge3(
            &Doc::from_str(input.base),
            &Doc::from_str(input.left),
            &Doc::from_str(input.right),
        );
        Ok(DriverOutput {
            merged: merge.merged.to_string(),
            conflicts: merge.conflicts,
            driver: self.name(),
        })
    }
}
