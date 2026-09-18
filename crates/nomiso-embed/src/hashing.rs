//! Deterministic bag-of-tokens hashing embedder (no network, no model weights).

use async_trait::async_trait;
use nomiso_core::error::{Error, Result};
use nomiso_service::Embedder;

/// Offline embedder: tokenizes on whitespace/punctuation, hashes into a fixed dim.
///
/// Similar texts share token mass → cosine/KNN works for paraphrase-ish fixtures
/// without calling an external model. Not a substitute for production embeddings.
#[derive(Debug, Clone)]
pub struct HashingEmbedder {
    dim: usize,
}

impl HashingEmbedder {
    /// Create with target dimension (must match store HNSW dim).
    pub fn new(dim: usize) -> Result<Self> {
        if dim == 0 {
            return Err(Error::invalid("embedding dim must be > 0"));
        }
        Ok(Self { dim })
    }

    /// Dimension.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Sync embed of a single string (used by async trait).
    pub fn embed_one(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; self.dim];
        for tok in tokenize(text) {
            let h = hash_token(&tok);
            let idx = (h as usize) % self.dim;
            // Independent sign bit: use high bits, NOT LSB of h.
            // LSB correlates with `h % dim` when dim is a power of two
            // (same parity coupling), which collapses orthants and warps KNN.
            let sign = if (h >> 32) & 1 == 0 { 1.0 } else { -1.0 };
            v[idx] += sign;
        }
        // L2 normalize for cosine geometry
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }
}

#[async_trait]
impl Embedder for HashingEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| self.embed_one(t)).collect())
    }

    fn identity(&self) -> Option<nomiso_core::EmbeddingIdentity> {
        Some(nomiso_core::EmbeddingIdentity {
            family: "nomiso-hashing".into(),
            model: "fnv1a-bow-v1".into(),
            dimension: self.dim as u32,
            normalization: nomiso_core::EmbeddingNormalization::L2,
            encoding: "f32".into(),
            limitation: Some(
                "offline bag-of-tokens hasher; not a production embedding model".into(),
            ),
        })
    }
}

fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn hash_token(tok: &str) -> u64 {
    // FNV-1a 64
    let mut h: u64 = 0xcbf29ce484222325;
    for b in tok.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn similar_texts_closer_than_unrelated() {
        let emb = HashingEmbedder::new(32).unwrap();
        let a = emb.embed_one("Alice prefers TypeScript for tooling");
        let b = emb.embed_one("Alice likes TypeScript tooling");
        let c = emb.embed_one("completely unrelated quantum barbecue");
        let sim = |x: &[f32], y: &[f32]| x.iter().zip(y).map(|(p, q)| p * q).sum::<f32>();
        assert!(
            sim(&a, &b) > sim(&a, &c),
            "paraphrase should beat unrelated: ab={} ac={}",
            sim(&a, &b),
            sim(&a, &c)
        );
    }

    #[tokio::test]
    async fn dimension_matches() {
        let emb = HashingEmbedder::new(16).unwrap();
        let out = emb.embed(&["hello world".into()]).await.unwrap();
        assert_eq!(out[0].len(), 16);
    }

    /// Sign must not be parity-coupled to index (dim power of two).
    #[test]
    fn sign_independent_of_index_parity() {
        // Collect (idx_parity, sign) over many tokens — both sign polarities
        // must appear for even and odd indices.
        let emb = HashingEmbedder::new(64).unwrap();
        let mut even_pos = 0u32;
        let mut even_neg = 0u32;
        let mut odd_pos = 0u32;
        let mut odd_neg = 0u32;
        // Raw (pre-normalize) inspection via re-implementing only the hash path
        // would re-test the hash; instead probe many single-token embeds and
        // check that non-zero components exist with mixed signs across parity.
        for i in 0..500 {
            let tok = format!("token{i}xyz");
            let v = emb.embed_one(&tok);
            for (idx, &x) in v.iter().enumerate() {
                if x == 0.0 {
                    continue;
                }
                let even_idx = idx % 2 == 0;
                if even_idx {
                    if x > 0.0 {
                        even_pos += 1;
                    } else {
                        even_neg += 1;
                    }
                } else if x > 0.0 {
                    odd_pos += 1;
                } else {
                    odd_neg += 1;
                }
            }
        }
        assert!(
            even_pos > 0 && even_neg > 0 && odd_pos > 0 && odd_neg > 0,
            "sign must be independent of index parity; got even(+{even_pos}/-{even_neg}) odd(+{odd_pos}/-{odd_neg})"
        );
    }
}
