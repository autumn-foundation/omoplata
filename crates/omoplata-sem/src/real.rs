//! Real transformer embeddings behind the [`Embedder`] trait, gated on the
//! `fastembed` Cargo feature (default **off**).
//!
//! This is the swap point promised by §5.7 / **P7** and ADR-0006 made concrete:
//! [`FastEmbedder`] is a learned sentence-embedding model
//! (`sentence-transformers/all-MiniLM-L6-v2`, 384-dim) served through the
//! [`fastembed`] crate, which uses ONNX Runtime for inference. Neither the model
//! weights nor the ONNX Runtime native library ship in this repo; both are
//! fetched on first construction:
//!
//! * the model (~87 MB `model.onnx` + tokenizer) from **huggingface.co**
//!   (`Qdrant/all-MiniLM-L6-v2-onnx`), and
//! * the ONNX Runtime static library from the **ort.pyke.io** CDN, linked at
//!   build time.
//!
//! Because that fetch can fail (offline, egress policy, disk), construction is
//! fallible — [`FastEmbedder::try_new`] returns [`SemError::Model`] rather than
//! panicking, so a caller can fall back to [`crate::HashingEmbedder`]. The
//! default build never compiles this module and never needs the model.
//!
//! Everything downstream ([`crate::search`], [`crate::find_duplicates`],
//! [`crate::cosine`]) is model-agnostic and works unchanged: swapping
//! `HashingEmbedder` for `FastEmbedder` upgrades lexical similarity to genuine
//! semantic similarity without touching those consumers.

use std::sync::Mutex;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use crate::embed::Embedder;
use crate::error::SemError;

/// The concrete model shipped behind this feature.
const MODEL: EmbeddingModel = EmbeddingModel::AllMiniLML6V2;

/// A real learned sentence-embedding model (`all-MiniLM-L6-v2`, 384-dim)
/// implementing [`Embedder`].
///
/// The underlying [`TextEmbedding`] runs ONNX inference and its `embed` takes
/// `&mut self`, while [`Embedder::embed`] is `&self`; the model is therefore
/// wrapped in a [`Mutex`] for interior mutability. Embedding is deterministic
/// for a given model, satisfying the [`Embedder`] contract.
pub struct FastEmbedder {
    model: Mutex<TextEmbedding>,
    dim: usize,
}

impl std::fmt::Debug for FastEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FastEmbedder")
            .field("model", &"all-MiniLM-L6-v2")
            .field("dim", &self.dim)
            .finish()
    }
}

impl FastEmbedder {
    /// Load the model, fetching the weights and ONNX Runtime on first use.
    ///
    /// The vector dimensionality is discovered by embedding a probe string once,
    /// so [`dim`](Embedder::dim) reflects the actual model rather than a
    /// hard-coded constant.
    ///
    /// # Errors
    ///
    /// Returns [`SemError::Model`] if the model or runtime cannot be fetched or
    /// initialized (e.g. no network / blocked egress), or if the probe embed
    /// fails. Callers should fall back to [`crate::HashingEmbedder`] in that
    /// case — the offline default.
    pub fn try_new() -> Result<Self, SemError> {
        let mut model = TextEmbedding::try_new(InitOptions::new(MODEL))
            .map_err(|e| SemError::Model(e.to_string()))?;
        // Discover the true output dimension with a single probe embed.
        let probe = model
            .embed(vec!["dimension probe"], None)
            .map_err(|e| SemError::Model(e.to_string()))?;
        let dim = probe.first().map(Vec::len).unwrap_or(0);
        if dim == 0 {
            return Err(SemError::Model(
                "model returned a zero-length embedding".to_string(),
            ));
        }
        Ok(Self {
            model: Mutex::new(model),
            dim,
        })
    }
}

impl Embedder for FastEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        // The `Embedder` contract is infallible; on the (rare) inference error
        // or a poisoned lock we degrade to the zero vector, which `cosine`
        // treats as maximally dissimilar — never a panic, never a bogus match.
        let Ok(mut model) = self.model.lock() else {
            return vec![0.0; self.dim];
        };
        match model.embed(vec![text], None) {
            Ok(mut v) if !v.is_empty() => v.swap_remove(0),
            _ => vec![0.0; self.dim],
        }
    }
}
