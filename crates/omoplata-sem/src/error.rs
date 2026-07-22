//! Error type for the semantic layer.

use omoplata_identity::IdentityError;

/// Errors raised by the semantic layer (§5.7).
///
/// Every fallible operation in this crate returns [`SemError`]; there is no
/// `unwrap`/`expect`/`panic` in non-test code, so a caller can always recover.
#[derive(Debug, thiserror::Error)]
pub enum SemError {
    /// Definition extraction (via `omoplata-identity`) failed — the source
    /// could not be parsed or the grammar could not be loaded.
    #[error("definition extraction failed: {0}")]
    Extraction(#[from] IdentityError),
}
