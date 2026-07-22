# ADR-0006: the embedding model is a deterministic local stand-in behind a pluggable `Embedder` trait

- Status: Accepted
- Date: 2026-07-22

## Context
The design doc puts a **semantic layer** on top of the substrate (§5.7): *"Every
node carries typed embeddings (AletheiaDB native) … Capabilities: semantic
bisect and archaeology …, review routing …, **duplicate-work detection**
(embedding-adjacent in-flight changes from different agents flagged before
textual collision — conflict avoidance, the cheapest tier of all)."* It is
listed as v1-in-scope in §8 — *"embeddings + duplicate-work detection"* — and
principle **P7** frames the mechanism: *"multiple typed embeddings per node …
Embeddings on every node come effectively free and power the semantic layer."*
The trust table (§7) classes `omoplata-sem` as **Unverified**; the doc names no
soundness invariant for this layer.

The obstacle is honesty about the model. **Real transformer embedding models are
not available offline in this environment**, and downloading model weights is out
of bounds. Producing a fake "AI" model and presenting it as a real one would be
dishonest; skipping the milestone would abandon the architecture the doc calls
for.

## Decision
Ship the **architecture**, not a model: typed embeddings per node plus
duplicate-work detection and semantic search over vector similarity, with the
model itself **pluggable** behind a trait.

- **`Embedder` trait** — `fn dim(&self) -> usize; fn embed(&self, text: &str) ->
  Vec<f32>`. This is the swap point (§5.7, P7): a real transformer model
  implements this trait and everything downstream keeps working unchanged.
- **`HashingEmbedder` — a deterministic local stand-in.** It uses **feature
  hashing** (the "hashing trick"): lowercase and tokenize the text, extract
  lexical features (word unigrams, word bigrams, and padded character trigrams
  for edit-robustness), hash each feature into `[0, dim)` with a hand-rolled
  **FNV-1a** hash (implemented in-crate, not a dependency, so bucketing is pinned
  and reproducible), accumulate a histogram, and **L2-normalize** to a unit
  vector. Same text ⇒ identical vector, on every platform and build.
- **Model-agnostic consumers.** `cosine` (zero-norm-guarded, clamped to
  `[-1, 1]`), `search` (top-k by cosine, deterministic index tie-break), and
  `find_duplicates` (all pairs at or above a threshold, sorted) read only
  vectors. Replacing the embedder changes *which* pairs are flagged, never these
  functions.
- **Two CLI verbs.** `omo dup <file.rs>...` embeds every definition across the
  given files and flags near-duplicate pairs (the "two agents implementing the
  same thing" detector); `omo similar <query> <file.rs>...` ranks definitions by
  similarity to a query.

## The reduction, stated plainly
**The shipped embedder is a stand-in, not a learned model.** It captures
*lexical* similarity — shared words and character n-grams — and calling it a
semantic model would be a lie. Concretely:

- It has **no semantic understanding**: two functions that do the same thing with
  entirely different vocabulary score low; two unrelated functions that share
  boilerplate score higher than they should. A real model would close both gaps.
- Thresholds (`omo dup` defaults to cosine ≥ 0.85) are tuned to *lexical*
  overlap, not meaning, and would be re-tuned for a real model.
- Every site that instantiates the fake embedder is marked `// NOTE (stand-in
  model)` in the source, and the module/type docs repeat the caveat.

The point of M7 is the **architecture** — typed embeddings per node, duplicate
detection and search over vector similarity, model behind a trait — proven out
end-to-end with a deterministic model that needs no network and no weights. Swap
`HashingEmbedder` for a real transformer implementation of `Embedder` and the
semantic layer becomes real with no change to its consumers.

## Consequences
- `omoplata-sem` is pure Rust with a single internal dependency
  (`omoplata-identity`, reused for definition extraction per **P6**) and no model
  runtime, so the crate builds and tests fully offline and deterministically.
- Duplicate-work detection and semantic search are **demonstrable today** on real
  Rust files, at reduced (lexical) quality — an honest floor, not a ceiling.
- **Future work.** A real embedding model behind `Embedder`; wiring the typed
  embeddings into the AletheiaDB substrate (§5.7 lists diff content, spec text,
  and definition signatures as distinct *typed* embeddings — here only the
  definition-signature embedding is materialized); and joining vector queries to
  the definition graph and revsets (§5.8) for semantic bisect and review
  routing, which this milestone does not attempt.
