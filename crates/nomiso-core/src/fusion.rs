//! Pure Reciprocal Rank Fusion for unit tests and offline fixtures.
//!
//! Production hybrid fusion uses SurrealDB `search::rrf` in-engine.
//! This module mirrors the same formula for deterministic unit tests.

use std::collections::HashMap;
use std::hash::Hash;

/// Fuse pre-sorted ranked lists with RRF: `score(d) = Σ 1/(k + rank_i(d))`.
///
/// * `lists` — each inner slice is most-relevant-first (rank 1 = first element).
/// * `k` — RRF constant (default 60 in literature / SurrealDB).
/// * Returns ids sorted by descending fused score (ties broken by first-seen order).
pub fn rrf_fuse<T>(lists: &[Vec<T>], k: u32) -> Vec<(T, f64)>
where
    T: Clone + Eq + Hash,
{
    let k = k as f64;
    let mut scores: HashMap<T, f64> = HashMap::new();
    let mut order: Vec<T> = Vec::new();

    for list in lists {
        for (idx, id) in list.iter().enumerate() {
            let rank = (idx + 1) as f64;
            let contrib = 1.0 / (k + rank);
            if let Some(s) = scores.get_mut(id) {
                *s += contrib;
            } else {
                scores.insert(id.clone(), contrib);
                order.push(id.clone());
            }
        }
    }

    let mut out: Vec<(T, f64)> = order
        .into_iter()
        .map(|id| {
            let score = scores[&id];
            (id, score)
        })
        .collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn rrf_prefers_consensus() {
        // id "a" is rank1 in both lists → highest fused score
        let lists = vec![vec!["a", "b", "c"], vec!["a", "c", "d"]];
        let fused = rrf_fuse(&lists, 60);
        assert_eq!(fused[0].0, "a");
        assert!(fused[0].1 > fused[1].1);
    }

    #[test]
    fn rrf_single_list() {
        let lists = vec![vec!["x", "y"]];
        let fused = rrf_fuse(&lists, 60);
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].0, "x");
        // 1/(60+1)
        assert!((fused[0].1 - 1.0 / 61.0).abs() < 1e-12);
    }

    #[test]
    fn rrf_empty() {
        let lists: Vec<Vec<&str>> = vec![];
        assert!(rrf_fuse(&lists, 60).is_empty());
    }

    #[test]
    fn rrf_known_formula() {
        // SurrealDB docs style: rank 1 → 1/(60+1)
        let lists = vec![vec!["doc1"], vec!["doc1", "doc2"]];
        let fused = rrf_fuse(&lists, 60);
        let s1 = 1.0 / 61.0 + 1.0 / 61.0;
        assert!((fused[0].1 - s1).abs() < 1e-12);
        assert_eq!(fused[0].0, "doc1");
    }
}
