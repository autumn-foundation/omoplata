//! Vector operations over embeddings — model-agnostic (they see only vectors).

/// Cosine similarity of two vectors, in `[-1, 1]`.
///
/// Returns `0.0` when either vector has zero norm (there is no orientation to
/// compare) or when the lengths differ (mismatched embedders) — a defensive
/// guard so a caller never divides by zero or reads out of bounds. The result
/// is clamped to `[-1, 1]` to absorb floating-point overshoot at the extremes.
///
/// For unit vectors (as produced by [`crate::HashingEmbedder`]) this reduces to
/// the dot product, so `cosine(v, v) == 1.0` up to rounding.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())).clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_vectors_score_one() {
        let v = vec![0.5, 0.5, 0.5, 0.5];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn orthogonal_vectors_score_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!(cosine(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn opposite_vectors_score_minus_one() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((cosine(&a, &b) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn zero_norm_is_zero() {
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 1.0];
        assert_eq!(cosine(&a, &b), 0.0);
    }

    #[test]
    fn length_mismatch_is_zero() {
        assert_eq!(cosine(&[1.0, 0.0], &[1.0]), 0.0);
    }

    #[test]
    fn result_is_bounded() {
        let a = vec![3.0, 4.0];
        let b = vec![6.0, 8.0];
        let c = cosine(&a, &b);
        assert!((-1.0..=1.0).contains(&c));
        assert!((c - 1.0).abs() < 1e-6);
    }
}
