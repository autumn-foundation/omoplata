//! The embedder: the `Embedder` trait and its deterministic local
//! implementation, [`HashingEmbedder`].
//!
//! # Stand-in model (read this)
//!
//! Real transformer embedding models are not available offline in this
//! environment, so the concrete embedder shipped here is a **deterministic
//! local feature-hashing stand-in**, not a learned model. It is documented in
//! `docs/adr/0006-semantic-embeddings.md`. The architectural point of §5.7 is
//! *typed embeddings per node* plus *duplicate-work detection over vector
//! similarity* with a **pluggable model** — the [`Embedder`] trait is that swap
//! point. A real transformer model drops in behind the trait without touching
//! [`crate::search`], [`crate::find_duplicates`], or [`crate::cosine`], which
//! are all model-agnostic (they see only vectors).

/// A model that maps text to a fixed-dimension embedding vector.
///
/// This is the design's swap point (§5.7, principle **P7**): any real
/// transformer embedding model implements this trait and everything downstream
/// — semantic search, duplicate-work detection — keeps working unchanged
/// because it operates only on the returned vectors.
pub trait Embedder {
    /// The dimensionality of the vectors this embedder produces.
    ///
    /// Every vector returned by [`embed`](Embedder::embed) has exactly this
    /// length.
    fn dim(&self) -> usize;

    /// Embed `text` into a vector of length [`dim`](Embedder::dim).
    ///
    /// Implementations must be **deterministic**: the same input text always
    /// yields the identical vector.
    fn embed(&self, text: &str) -> Vec<f32>;
}

/// The 64-bit FNV-1a offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// The 64-bit FNV-1a prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a hash of `bytes` — a small, fixed, dependency-free hash so feature
/// bucketing is deterministic and reproducible across builds and platforms.
///
/// Implemented here deliberately (rather than pulling in a crate) so the
/// hashing is pinned and cannot drift with a dependency bump.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// A deterministic local embedder using **feature hashing** (the "hashing
/// trick") over lexical features, L2-normalized to a unit vector.
///
/// # NOTE (stand-in model)
///
/// This is the documented stand-in for a real embedding model (see the module
/// docs and ADR-0006). It captures *lexical* similarity only — shared words,
/// word bigrams, and character n-grams — which is enough to demonstrate the
/// §5.7 architecture (typed embeddings + duplicate-work detection) but is **not**
/// semantic understanding. A real model replaces it behind [`Embedder`].
///
/// Features extracted from the lowercased text:
/// * **word unigrams** — each alphanumeric token;
/// * **word bigrams** — each adjacent token pair (a little word-order signal);
/// * **character trigrams** — padded 3-grams within each token, which make the
///   embedding robust to small edits (a renamed identifier still shares most of
///   its trigrams).
///
/// Each feature is hashed with [`fnv1a`] into `[0, dim)` and its count
/// accumulated; the resulting histogram is then L2-normalized so that
/// [`crate::cosine`] measures orientation, not magnitude.
#[derive(Debug, Clone)]
pub struct HashingEmbedder {
    dim: usize,
}

/// The default embedding dimensionality.
pub const DEFAULT_DIM: usize = 256;

impl Default for HashingEmbedder {
    fn default() -> Self {
        Self::new(DEFAULT_DIM)
    }
}

impl HashingEmbedder {
    /// Create a hashing embedder producing vectors of dimension `dim`.
    ///
    /// A `dim` of 0 is clamped to 1 so the returned vector is never empty and
    /// bucketing never divides by zero.
    pub fn new(dim: usize) -> Self {
        Self { dim: dim.max(1) }
    }

    /// Accumulate one feature string into the histogram `buckets`.
    fn add_feature(&self, buckets: &mut [f32], feature: &str) {
        // `self.dim` is >= 1, so the modulus is well-defined.
        let idx = (fnv1a(feature.as_bytes()) % self.dim as u64) as usize;
        buckets[idx] += 1.0;
    }
}

/// Tokenize `text` into lowercase alphanumeric tokens, splitting on every
/// non-alphanumeric character. Empty tokens are dropped.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Emit padded character trigrams of `token` into `out` (as `c:<trigram>`).
///
/// The token is padded with a leading `^` and trailing `$` so prefixes and
/// suffixes are represented; short tokens still emit at least one trigram.
fn char_trigrams(token: &str, out: &mut Vec<String>) {
    let mut chars: Vec<char> = Vec::with_capacity(token.chars().count() + 2);
    chars.push('^');
    chars.extend(token.chars());
    chars.push('$');
    for window in chars.windows(3) {
        let mut feature = String::from("c:");
        feature.extend(window.iter());
        out.push(feature);
    }
}

impl Embedder for HashingEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let mut buckets = vec![0.0f32; self.dim];
        let tokens = tokenize(text);

        // Word unigrams and character trigrams.
        for token in &tokens {
            self.add_feature(&mut buckets, &format!("w:{token}"));
            let mut grams = Vec::new();
            char_trigrams(token, &mut grams);
            for gram in &grams {
                self.add_feature(&mut buckets, gram);
            }
        }
        // Word bigrams (adjacent token pairs).
        for pair in tokens.windows(2) {
            self.add_feature(&mut buckets, &format!("b:{}_{}", pair[0], pair[1]));
        }

        l2_normalize(&mut buckets);
        buckets
    }
}

/// L2-normalize `v` in place to a unit vector.
///
/// If `v` has zero norm (e.g. the text produced no features), it is left as the
/// zero vector — there is no unit direction to normalize to, and downstream
/// [`crate::cosine`] treats a zero vector as maximally dissimilar.
fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_same_text_same_vector() {
        let e = HashingEmbedder::default();
        let a = e.embed("fn area(width: f64, height: f64) -> f64 { width * height }");
        let b = e.embed("fn area(width: f64, height: f64) -> f64 { width * height }");
        assert_eq!(a, b);
    }

    #[test]
    fn dimension_matches_dim() {
        let e = HashingEmbedder::new(128);
        assert_eq!(e.dim(), 128);
        assert_eq!(e.embed("hello world").len(), 128);
    }

    #[test]
    fn nonempty_text_is_unit_norm() {
        let e = HashingEmbedder::default();
        let v = e.embed("the quick brown fox");
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm was {norm}");
    }

    #[test]
    fn empty_text_is_zero_vector() {
        let e = HashingEmbedder::default();
        let v = e.embed("");
        assert!(v.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn dim_zero_is_clamped() {
        let e = HashingEmbedder::new(0);
        assert_eq!(e.dim(), 1);
        assert_eq!(e.embed("x").len(), 1);
    }
}
