//! omoplata's semantic layer (design doc §7 crate #7, `omoplata-sem`).
//!
//! The design doc's §5.7 pins a **semantic layer** on top of the substrate:
//!
//! > **§5.7 Semantic layer.** Every node carries typed embeddings (AletheiaDB
//! > native): diff content, change description/spec text, touched-definition
//! > signatures — independently indexed. Capabilities: semantic bisect and
//! > archaeology …, review routing …, **duplicate-work detection** (embedding-
//! > adjacent in-flight changes from different agents flagged *before* textual
//! > collision — conflict avoidance, the cheapest tier of all), and provenance
//! > queries …
//!
//! and lists it as v1-in-scope in §8: *"embeddings + duplicate-work
//! detection"*. Principle **P7** frames the mechanism: *"multiple typed
//! embeddings per node … Embeddings on every node come effectively free and
//! power the semantic layer."* The architecture diagram (§4) names the substrate
//! node **`EM[Typed embeddings per node]`**, and Tier 3 (§4) consumes
//! *"embedding-derived context"*.
//!
//! This crate implements that architecture with a **pluggable model**:
//!
//! * [`Embedder`] — the trait every embedding model implements; the swap point.
//! * [`HashingEmbedder`] — a **deterministic local stand-in** (see below); the
//!   offline default.
//! * `FastEmbedder` — a **real** learned transformer model (`all-MiniLM-L6-v2`),
//!   gated behind the opt-in `fastembed` Cargo feature (default off; fetches the
//!   model from HuggingFace and ONNX Runtime from the ort CDN on first use).
//! * [`Embedded<T>`] — "typed embeddings per node": an item paired with its
//!   vector.
//! * [`embed_definitions`] — extract Rust definitions (via `omoplata-identity`,
//!   **P6**) and embed each, giving a per-definition typed embedding.
//! * [`cosine`] — model-agnostic vector similarity.
//! * [`search`] — semantic search (top-k by cosine to a query).
//! * [`find_duplicates`] — **duplicate-work detection** (§8): all corpus pairs
//!   above a similarity threshold — "two agents implementing the same thing".
//!
//! # Model honesty (stand-in)
//!
//! The **default** build ships only the [`HashingEmbedder`], a **deterministic
//! feature-hashing stand-in** (not a learned model — it captures lexical
//! similarity only), so the crate builds and tests fully offline with no model
//! and no network. A **real** transformer model (`FastEmbedder`) is available as
//! the opt-in `fastembed` feature, which fetches the weights and ONNX Runtime on
//! first use; it is off by default so the offline path stays the default. This
//! is documented in `docs/adr/0006-semantic-embeddings.md`. The *point* of this crate is the
//! architecture (typed embeddings per node + duplicate detection over vector
//! similarity) with the model behind [`Embedder`] as the swap point; every
//! consumer here ([`search`], [`find_duplicates`], [`cosine`]) is model-agnostic
//! and works unchanged once a real model is dropped in. `// NOTE (stand-in
//! model)` marks each site the fake embedder is instantiated.
//!
//! # Verification status
//!
//! `omoplata-sem` is **Unverified** in the design's trust table (§7). It carries
//! no soundness invariant; the doc names no invariant for this layer. There is
//! no `unwrap`/`expect`/`panic` in non-test code.

mod embed;
mod error;
#[cfg(feature = "fastembed")]
mod real;
mod vector;

pub use embed::{Embedder, HashingEmbedder, DEFAULT_DIM};
pub use error::SemError;
#[cfg(feature = "fastembed")]
pub use real::FastEmbedder;
pub use vector::cosine;

use std::path::{Path, PathBuf};

use omoplata_identity::{extract_definitions, Definition};

/// Location of a definition within a workspace or file scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDefLocation {
    /// Workspace name (e.g. `Some("w1")`), or `None` if scanning explicit files.
    pub workspace: Option<String>,
    /// File path relative to workspace or repository root.
    pub file_path: PathBuf,
    /// The extracted definition.
    pub def: Definition,
}

/// A value paired with its embedding vector — "typed embeddings per node"
/// (§5.7).
///
/// `T` is the node type (here, a [`Definition`], but the container is generic so
/// any node — a change, a diff, a spec — can carry a typed embedding).
#[derive(Debug, Clone, PartialEq)]
pub struct Embedded<T> {
    /// The embedded item.
    pub item: T,
    /// Its embedding vector, of length `embedder.dim()`.
    pub vector: Vec<f32>,
}

/// The text embedded for a definition: its kind label, name, and source body
/// (§5.7 "touched-definition signatures").
///
/// The `byte_range` slice already contains the signature and body; the kind
/// label and name are prepended so those signals are weighted a little more.
/// If the range is out of bounds (it should not be for a range produced by the
/// same source), only the kind and name are used.
fn definition_text(source: &str, def: &Definition) -> String {
    let body = source.get(def.byte_range.clone()).unwrap_or("");
    format!("{} {} {}", def.kind.label(), def.name, body)
}

/// Extract every Rust definition from `source` and embed each one, yielding a
/// per-definition typed embedding (§5.7, **P6**).
///
/// Each definition is embedded from its kind, name, and source body (see
/// [`definition_text`]).
///
/// # Errors
///
/// Returns [`SemError::Extraction`] if the source cannot be parsed or the
/// tree-sitter grammar cannot be loaded (propagated from `omoplata-identity`).
pub fn embed_definitions<E: Embedder + ?Sized>(
    embedder: &E,
    source: &str,
) -> Result<Vec<Embedded<Definition>>, SemError> {
    let defs = extract_definitions(source)?;
    Ok(defs
        .into_iter()
        .map(|def| {
            let vector = embedder.embed(&definition_text(source, &def));
            Embedded { item: def, vector }
        })
        .collect())
}

/// Recursively scan `dir` for `.rs` files (skipping `.omoplata` and `.git`),
/// extract definitions, and embed each one.
///
/// Yields `Embedded<WorkspaceDefLocation>` items tagging each definition with its
/// workspace name and file path relative to `dir`.
///
/// # Errors
///
/// Returns [`SemError`] if directory traversal fails.
pub fn embed_workspace_dir<E: Embedder + ?Sized>(
    embedder: &E,
    workspace_name: Option<&str>,
    dir: &Path,
) -> Result<Vec<Embedded<WorkspaceDefLocation>>, SemError> {
    let mut results = Vec::new();
    if !dir.exists() {
        return Ok(results);
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str == ".omoplata" || name_str == ".git" {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
                if let Ok(source) = std::fs::read_to_string(&path) {
                    if let Ok(defs) = extract_definitions(&source) {
                        let rel_path = path.strip_prefix(dir).unwrap_or(&path).to_path_buf();
                        for def in defs {
                            let vector = embedder.embed(&definition_text(&source, &def));
                            results.push(Embedded {
                                item: WorkspaceDefLocation {
                                    workspace: workspace_name.map(String::from),
                                    file_path: rel_path.clone(),
                                    def,
                                },
                                vector,
                            });
                        }
                    }
                }
            }
        }
    }
    Ok(results)
}

/// Semantic search: the top-`k` corpus indices most similar to `query`.
///
/// The query is embedded with `embedder`, compared to every corpus vector by
/// [`cosine`], and the results are returned as `(index, score)` sorted by score
/// descending with a deterministic tie-break by ascending index. At most `k`
/// results are returned (fewer if the corpus is smaller); `k == 0` returns an
/// empty vector.
///
/// The corpus vectors must have been produced by the *same* embedder (same
/// dimension) as `embedder`; a dimension mismatch scores `0.0` for that entry
/// (see [`cosine`]).
pub fn search<E: Embedder + ?Sized, T>(
    embedder: &E,
    query: &str,
    corpus: &[Embedded<T>],
    k: usize,
) -> Vec<(usize, f32)> {
    let q = embedder.embed(query);
    let mut scored: Vec<(usize, f32)> = corpus
        .iter()
        .enumerate()
        .map(|(i, e)| (i, cosine(&q, &e.vector)))
        .collect();
    // Score descending; deterministic tie-break by ascending index.
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.truncate(k);
    scored
}

/// **Duplicate-work detection** (§8): every corpus pair `(i, j)` with `i < j`
/// whose embeddings have cosine similarity `>= threshold`.
///
/// This is the design's "two agents implementing the same thing" detector — the
/// cheapest tier of conflict avoidance (§5.7): embedding-adjacent work flagged
/// *before* it collides textually. Results are `(i, j, score)` sorted by score
/// descending, then by `(i, j)` ascending for a deterministic order.
///
/// The comparison is model-agnostic: it reads only the stored vectors, so a real
/// embedding model behind [`Embedder`] changes *which* pairs are flagged, not
/// this function.
pub fn find_duplicates<T>(corpus: &[Embedded<T>], threshold: f32) -> Vec<(usize, usize, f32)> {
    let mut pairs: Vec<(usize, usize, f32)> = Vec::new();
    for i in 0..corpus.len() {
        for j in (i + 1)..corpus.len() {
            let score = cosine(&corpus[i].vector, &corpus[j].vector);
            if score >= threshold {
                pairs.push((i, j, score));
            }
        }
    }
    // Score descending, then (i, j) ascending — fully deterministic.
    pairs.sort_by(|a, b| {
        b.2.total_cmp(&a.2)
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
    });
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A near-duplicate: identical body, one identifier (the fn name) renamed.
    const SUM_ALPHA: &str = "fn alpha(items: &[i32]) -> i32 {\n    let mut total = 0;\n    for value in items {\n        total += value;\n    }\n    total\n}\n";
    const SUM_BETA: &str = "fn beta(items: &[i32]) -> i32 {\n    let mut total = 0;\n    for value in items {\n        total += value;\n    }\n    total\n}\n";
    /// An unrelated definition.
    const GREET: &str = "fn greet(name: &str) -> String {\n    format!(\"hello, {name}!\")\n}\n";

    #[test]
    fn cosine_of_vector_with_itself_is_one() {
        // NOTE (stand-in model): deterministic hashing embedder.
        let e = HashingEmbedder::default();
        let v = e.embed(SUM_ALPHA);
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn near_duplicate_scores_high_unrelated_scores_low() {
        // NOTE (stand-in model): deterministic hashing embedder.
        let e = HashingEmbedder::default();
        let a = e.embed(SUM_ALPHA);
        let b = e.embed(SUM_BETA);
        let g = e.embed(GREET);

        let near = cosine(&a, &b);
        let unrelated = cosine(&a, &g);
        assert!(near > 0.8, "near-duplicate cosine was {near}");
        assert!(unrelated < 0.5, "unrelated cosine was {unrelated}");
        assert!(near > unrelated);
    }

    #[test]
    fn embed_definitions_yields_one_vector_per_definition() {
        // NOTE (stand-in model): deterministic hashing embedder.
        let e = HashingEmbedder::new(64);
        let src = format!("{SUM_ALPHA}{GREET}");
        let corpus = embed_definitions(&e, &src).unwrap();
        assert_eq!(corpus.len(), 2);
        for entry in &corpus {
            assert_eq!(entry.vector.len(), 64);
        }
        assert_eq!(corpus[0].item.name, "alpha");
        assert_eq!(corpus[1].item.name, "greet");
    }

    #[test]
    fn find_duplicates_flags_the_near_pair_only() {
        // NOTE (stand-in model): deterministic hashing embedder.
        let e = HashingEmbedder::default();
        // Two near-identical functions (alpha/beta) plus an unrelated one.
        let src = format!("{SUM_ALPHA}{SUM_BETA}{GREET}");
        let corpus = embed_definitions(&e, &src).unwrap();
        assert_eq!(corpus.len(), 3);

        let dups = find_duplicates(&corpus, 0.8);
        assert_eq!(
            dups.len(),
            1,
            "expected exactly one duplicate pair: {dups:?}"
        );
        let (i, j, score) = dups[0];
        // The pair is the two summing functions (indices 0 and 1), not greet (2).
        assert_eq!((i, j), (0, 1));
        assert!(score > 0.8);
        // greet is paired with neither above threshold.
        assert!(!dups.iter().any(|&(a, b, _)| a == 2 || b == 2));
    }

    #[test]
    fn search_ranks_the_close_item_first() {
        // NOTE (stand-in model): deterministic hashing embedder.
        let e = HashingEmbedder::default();
        let src = format!("{SUM_ALPHA}{GREET}");
        let corpus = embed_definitions(&e, &src).unwrap();

        // A query lexically close to `greet`.
        let hits = search(&e, "greet name hello format", &corpus, 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].0, 1,
            "expected greet (index 1) ranked first: {hits:?}"
        );
        assert!(hits[0].1 > hits[1].1);
    }

    #[test]
    fn search_k_zero_is_empty() {
        let e = HashingEmbedder::default();
        let corpus = embed_definitions(&e, SUM_ALPHA).unwrap();
        assert!(search(&e, "anything", &corpus, 0).is_empty());
    }

    #[test]
    fn search_is_deterministic_tie_break_by_index() {
        // Two identical definitions embed to the same vector; a query equal to
        // them ties, and the tie must break to the lower index.
        let e = HashingEmbedder::default();
        let src = format!("{SUM_ALPHA}{SUM_ALPHA}");
        let corpus = embed_definitions(&e, &src).unwrap();
        // Both definitions have identical bodies; only their names collide too
        // (both `alpha`), so their vectors are identical.
        let hits = search(&e, &definition_text(&src, &corpus[0].item), &corpus, 2);
        assert_eq!(hits[0].0, 0);
        assert_eq!(hits[1].0, 1);
    }
}
