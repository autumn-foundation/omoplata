# ADR-0006: a deterministic local stand-in is the default embedder, with real transformer embeddings available behind an opt-in feature — both behind a pluggable `Embedder` trait

- Status: Accepted
- Date: 2026-07-22
- Updated: 2026-07-22 — real embeddings (`fastembed` feature) added; see "Real embeddings, evidenced" below.

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

The obstacle is honesty about the model. A learned transformer model needs its
**weights fetched** (typically from HuggingFace) and an **inference runtime**
(here ONNX Runtime, itself fetched from a CDN). The default build must not depend
on either — it has to build and test **fully offline and deterministically** —
and producing a fake "AI" model while presenting it as a real one would be
dishonest. So the default embedder is a deterministic stand-in; a **real** model
is offered only as an explicit opt-in, and only because its hosts turned out to
be reachable here (see "Real embeddings, evidenced").

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

## Real embeddings, evidenced (`fastembed` feature)
The "swap point" is no longer hypothetical. The two hosts a real model needs —
HuggingFace (weights) and the ONNX Runtime CDN — were **probed from this
environment and found reachable through the egress proxy**, so a real model is
now wired in behind the trait as an **opt-in Cargo feature**, off by default.

Verbatim reachability (2026-07-22, through the agent proxy):

- `GET https://huggingface.co/` → **200**.
- `HEAD https://huggingface.co/Qdrant/all-MiniLM-L6-v2-onnx/resolve/main/model.onnx`
  → **302** redirect to `https://us.aws.cdn.hf.co/…`; a ranged `GET` of that
  redirect returns **206** with real bytes (`x-repo-commit`
  `5f1b8cd78bc4fb444dd171e59b18f3a3af89a079`).
- ONNX Runtime native library: fetched by the `ort` crate from the
  **`ort.pyke.io`** CDN (cached at `~/.cache/ort.pyke.io/dfbin/…/libonnxruntime.a`)
  and linked at build time — succeeded here.

Concretely added:

- **Cargo feature `fastembed`** on `omoplata-sem` (`default = []`, so the
  deterministic `HashingEmbedder` stays the offline default). It pulls in
  `fastembed = "5"` (which uses `ort` / ONNX Runtime) only when enabled.
- **`FastEmbedder`** — a real learned model,
  `sentence-transformers/all-MiniLM-L6-v2` (**384-dim**, `model.onnx` ≈ **87 MB**
  plus a ~700 KB tokenizer, downloaded once from HuggingFace on first use),
  implementing the same `Embedder` trait (`dim`, `embed`). The model loads
  lazily; construction is **fallible** (`SemError::Model`) so a caller that
  cannot reach the hosts falls back to the hashing stand-in rather than crashing.
  The default build never compiles it and never needs the model.
- **CLI `--real-embeddings`** on `omo dup` / `omo similar`: uses `FastEmbedder`
  when the binary was built with `--features fastembed` and the model is
  fetchable, otherwise prints a clear note and uses the hashing stand-in. Default
  behavior (no flag) is byte-for-byte unchanged.
- A **feature-gated integration test** (`#[cfg(feature = "fastembed")]`) asserts
  the real model separates a semantic paraphrase pair from an unrelated sentence
  by a wide margin.

Why it matters, measured. On three definitions — `total` and `accumulate` (same
logic, *different* vocabulary) plus an unrelated `greet` — the stand-in and the
real model disagree exactly where the ADR predicted:

| pair | hashing stand-in | real model |
| --- | --- | --- |
| `total ~ accumulate` (true semantic dup) | 0.35 | **0.72** |
| `total ~ greet` (unrelated) | **0.39** | 0.09 |
| `accumulate ~ greet` (unrelated) | 0.35 | 0.15 |

The stand-in ranks the *unrelated* `total ~ greet` pair (0.39) **above** the true
duplicate (0.35) — the boilerplate-collision failure mode named above. The real
model puts the true duplicate on top (0.72) and pushes both unrelated pairs down
(<0.16): genuine semantic separation the lexical model cannot produce. Because
`cosine`, `search`, and `find_duplicates` read only vectors, none of them
changed — only *which* pairs are flagged did.

## Consequences
- The **default** build of `omoplata-sem` is pure Rust with a single internal
  dependency (`omoplata-identity`, reused for definition extraction per **P6**)
  and no model runtime, so it builds and tests fully offline and
  deterministically. The real model is strictly opt-in (`--features fastembed`);
  turning the feature off restores the offline default with no behavioral change.
- Duplicate-work detection and semantic search are **demonstrable today** on real
  Rust files — at reduced (lexical) quality by default, and at genuine semantic
  quality with `--real-embeddings` where the model hosts are reachable (proven
  here; also true of CI/GitHub Actions with open internet).
- **Future work.** Wiring the typed embeddings into the AletheiaDB substrate
  (§5.7 lists diff content, spec text, and definition signatures as distinct
  *typed* embeddings — here only the definition-signature embedding is
  materialized); joining vector queries to the definition graph and revsets
  (§5.8) for semantic bisect and review routing, which this milestone does not
  attempt; and a pure-Rust **vendored-weights** path (e.g. model2vec static
  embeddings) so real semantic quality is available with *no* network at
  all — assessed as a viable follow-up, not implemented here.
