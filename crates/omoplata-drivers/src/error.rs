//! Error type for the driver layer.

/// Errors raised by a [`MergeDriver`](crate::MergeDriver).
///
/// The drivers in this crate are *untrusted proposers* (design doc §4, principle
/// **P1**): a bad or failed driver may return an error or a degraded conflict,
/// never a silently wrong merge — the verified kernel is what admits results.
///
/// The [`RustStructuralDriver`](crate::RustStructuralDriver) does not surface
/// [`Parse`](DriverError::Parse) in normal operation: when tree-sitter cannot
/// parse an input as Rust it falls back to the [`LineDriver`](crate::LineDriver)
/// rather than failing (see [`RustStructuralDriver`](crate::RustStructuralDriver)
/// for the documented fallback). The variant is retained so a caller that
/// prefers a hard failure can reuse the same `Result` shape.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    /// A source input could not be parsed by the structural driver's grammar.
    #[error("structural driver could not parse Rust source: {0}")]
    Parse(#[from] omoplata_identity::IdentityError),
}
