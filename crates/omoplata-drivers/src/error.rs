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
///
/// The [`MergirafDriver`](crate::MergirafDriver) surfaces
/// [`Tool`](DriverError::Tool) when the external `mergiraf` binary is missing,
/// cannot be spawned, times out, or exits abnormally — failures of an out-of-
/// process proposer are made visible rather than silently degraded.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    /// A source input could not be parsed by the structural driver's grammar.
    #[error("structural driver could not parse Rust source: {0}")]
    Parse(#[from] omoplata_identity::IdentityError),

    /// An external merge tool (Mergiraf) failed: absent, unspawnable, timed
    /// out, or exited with an unexpected status.
    #[error("external merge tool failed: {0}")]
    Tool(String),
}

impl DriverError {
    /// Construct a [`Tool`](DriverError::Tool) error from any displayable
    /// message.
    pub(crate) fn tool(msg: impl Into<String>) -> Self {
        DriverError::Tool(msg.into())
    }
}
