//! Integration test for the real transformer embedder, gated on the `fastembed`
//! feature. It is compiled and run only with:
//!
//! ```text
//! cargo test -p omoplata-sem --features fastembed
//! ```
//!
//! The default `cargo test --all` (feature off) does not compile this file, so
//! the offline test suite needs no model and no network.
//!
//! The assertion is the whole point of a *real* model over the lexical stand-in:
//! two sentences that mean the same thing with **different vocabulary** must
//! score clearly higher than an unrelated sentence — something feature hashing
//! cannot do, since it sees no shared words.
#![cfg(feature = "fastembed")]

use omoplata_sem::{cosine, Embedder, FastEmbedder};

#[test]
fn real_model_separates_semantic_paraphrase_from_unrelated() {
    let embedder = match FastEmbedder::try_new() {
        Ok(e) => e,
        Err(e) => panic!(
            "could not load the real embedding model (network/egress?): {e}. \
             Run with --features fastembed only where the model is fetchable."
        ),
    };

    // 384-dim for all-MiniLM-L6-v2.
    assert_eq!(embedder.dim(), 384);

    // A paraphrase pair with almost no shared words, plus an unrelated sentence.
    let a = embedder.embed("The cat sat quietly on the warm windowsill.");
    let b = embedder.embed("A feline rested calmly by the sunny window.");
    let unrelated = embedder.embed("Quarterly revenue growth exceeded market expectations.");

    let paraphrase = cosine(&a, &b);
    let off_topic = cosine(&a, &unrelated);

    // The real model must place the paraphrase well above the unrelated pair.
    assert!(
        paraphrase > 0.4,
        "paraphrase cosine unexpectedly low: {paraphrase}"
    );
    assert!(
        off_topic < 0.2,
        "unrelated cosine unexpectedly high: {off_topic}"
    );
    assert!(
        paraphrase - off_topic > 0.3,
        "insufficient separation: paraphrase={paraphrase}, off_topic={off_topic}"
    );
}

#[test]
fn real_model_is_deterministic() {
    let embedder = FastEmbedder::try_new().expect("model load");
    let x = embedder.embed("deterministic output check");
    let y = embedder.embed("deterministic output check");
    assert_eq!(x, y);
}
