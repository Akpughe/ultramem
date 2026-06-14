//! Sparse (lexical) vectors for hybrid search. Dense embeddings miss exact
//! terms — rare tokens, IDs, error codes, proper nouns the embedding model
//! blurs together. A BM25-style sparse vector restores that lexical channel;
//! fused with the dense channel via RRF in Qdrant, it catches what each misses.
//!
//! We send raw term frequencies (token-id → count) and let Qdrant apply IDF
//! server-side (the collection's sparse vector carries `modifier: "idf"`), so
//! there's no corpus statistics to maintain on our side — Qdrant's BM25.

use std::collections::HashMap;

/// Hash a token to a stable 32-bit id (FNV-1a). Collisions are astronomically
/// rare at this vocabulary size and harmless to ranking.
fn token_id(tok: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in tok.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Lowercased alphanumeric tokens of length ≥2. Mirrors the dense side's
/// tolerance for separator-heavy identifiers.
fn tokenize(text: &str) -> impl Iterator<Item = String> + '_ {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(String::from)
        .collect::<Vec<_>>()
        .into_iter()
}

/// Build a sparse vector as `(indices, values)` of token-id → term frequency.
/// Empty when the text has no usable tokens.
pub fn sparse_vector(text: &str) -> (Vec<u32>, Vec<f32>) {
    let mut counts: HashMap<u32, f32> = HashMap::new();
    for tok in tokenize(text) {
        *counts.entry(token_id(&tok)).or_insert(0.0) += 1.0;
    }
    let mut pairs: Vec<(u32, f32)> = counts.into_iter().collect();
    // Stable order (by index) — not required by Qdrant but keeps payloads
    // deterministic and tests simple.
    pairs.sort_by_key(|(i, _)| *i);
    pairs.into_iter().unzip()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn term_frequencies_count_repeats() {
        let (idx, val) = sparse_vector("alpha beta alpha");
        assert_eq!(idx.len(), 2, "two distinct tokens");
        // alpha appears twice → its value is 2.0
        let alpha = token_id("alpha");
        let pos = idx.iter().position(|i| *i == alpha).unwrap();
        assert_eq!(val[pos], 2.0);
    }

    #[test]
    fn drops_short_and_nonalnum_tokens() {
        let (idx, _) = sparse_vector("a, I! the-quick brown");
        // "a" and "I" are length<2 and dropped; "the", "quick", "brown" kept.
        assert_eq!(idx.len(), 3);
    }

    #[test]
    fn empty_text_is_empty_vector() {
        let (idx, val) = sparse_vector("   !!!  ");
        assert!(idx.is_empty() && val.is_empty());
    }

    #[test]
    fn token_id_is_stable() {
        assert_eq!(token_id("recally"), token_id("recally"));
        assert_ne!(token_id("recally"), token_id("qdrant"));
    }
}
