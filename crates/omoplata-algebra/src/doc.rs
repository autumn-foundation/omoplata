//! The line-oriented document model.
//!
//! [`Doc`] is the opaque/line layer of the design doc's two-layer patch algebra
//! (§5.2): "for opaque blobs, byte/line Myers with a fixed leftmost-topmost
//! tie-break". A document is an ordered sequence of lines; all diff, apply,
//! commutation, and merge operations in this crate are defined over it.
//!
//! The definition-level (tree-sitter) layer of §5.2 is a later milestone; this
//! layer is drawn so it can sit underneath that one without API changes.

/// An ordered sequence of text lines.
///
/// A `Doc` is the atom the algebra operates on. It is constructed from a string
/// by splitting on `'\n'` and rendered back by joining on `'\n'`, so the
/// round-trip [`Doc::from_str`] → [`Doc::to_string`] is exactly the identity on
/// the original bytes (see the `roundtrip` tests). This faithful round-trip is
/// what lets the line layer stand in for the design doc's "faithful diff over a
/// snapshot pair" without losing or inventing content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Doc {
    lines: Vec<String>,
}

impl Doc {
    /// Build a document from its ordered lines directly.
    #[must_use]
    pub fn from_lines(lines: Vec<String>) -> Self {
        Self { lines }
    }

    /// Parse a document from a string by splitting on `'\n'`.
    ///
    /// The split is content-faithful: `"a\nb"` becomes `["a", "b"]`, `"a\n"`
    /// becomes `["a", ""]`, and `""` becomes `[""]`. Joining the result on
    /// `'\n'` reproduces the input byte-for-byte, so
    /// [`from_str`](Doc::from_str) followed by [`to_string`](Doc::to_string) is
    /// the identity function.
    ///
    /// This is deliberately named `from_str` (an inherent method) rather than
    /// implementing [`std::str::FromStr`], because parsing is total — it never
    /// fails — and an inherent method reads more clearly at call sites.
    #[must_use]
    #[allow(clippy::should_implement_trait)] // total parse; see doc comment above
    pub fn from_str(s: &str) -> Self {
        Self {
            lines: s.split('\n').map(str::to_owned).collect(),
        }
    }

    /// The document's lines, in order.
    #[must_use]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// The number of lines in the document.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether the document has no lines. A `Doc` parsed from a string always
    /// has at least one line, so this is only true for a directly-constructed
    /// empty document.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

impl std::fmt::Display for Doc {
    /// Render the document by joining its lines on `'\n'`. The exact inverse of
    /// [`Doc::from_str`].
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.lines.join("\n"))
    }
}
