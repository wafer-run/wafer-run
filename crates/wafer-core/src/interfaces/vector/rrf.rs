//! Reciprocal Rank Fusion for combining ranked result lists.
//!
//! Given N ranked lists of document IDs, compute
//! `score(d) = sum over lists of 1 / (k + rank_in_list(d))`
//! for each document that appears in any list, then sort descending.

use std::collections::BTreeMap;

pub const DEFAULT_RRF_K: f32 = 60.0;

/// Fuse multiple ranked ID lists into a single ranked list via RRF.
///
/// `rankings[i]` is an ordered list of document IDs (best first).
/// Returns up to `top_k` IDs sorted by fused score descending.
pub fn fuse(rankings: &[Vec<String>], top_k: usize, k: f32) -> Vec<String> {
    let mut scores: BTreeMap<String, f32> = BTreeMap::new();
    for list in rankings {
        for (rank, id) in list.iter().enumerate() {
            let rank_1based = (rank + 1) as f32;
            *scores.entry(id.clone()).or_insert(0.0) += 1.0 / (k + rank_1based);
        }
    }
    let mut pairs: Vec<(String, f32)> = scores.into_iter().collect();
    pairs.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    pairs.into_iter().take(top_k).map(|(id, _)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(slice: &[&str]) -> Vec<String> {
        slice.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn single_list_preserves_order() {
        let r = ids(&["a", "b", "c"]);
        assert_eq!(fuse(&[r], 10, DEFAULT_RRF_K), ids(&["a", "b", "c"]));
    }

    #[test]
    fn two_lists_boost_common_items() {
        let a = ids(&["x", "y", "z"]);
        let b = ids(&["y", "w", "x"]);
        let fused = fuse(&[a, b], 10, DEFAULT_RRF_K);
        // y appears at rank 2 in list a (1/62) + rank 1 in list b (1/61) ≈ 0.0327
        // x appears at rank 1 in a (1/61) + rank 3 in b (1/63) ≈ 0.0323
        // y edges out x.
        assert_eq!(fused[0], "y");
        assert_eq!(fused[1], "x");
    }

    #[test]
    fn respects_top_k() {
        let a = ids(&["a", "b", "c", "d", "e"]);
        let fused = fuse(&[a], 3, DEFAULT_RRF_K);
        assert_eq!(fused, ids(&["a", "b", "c"]));
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let empty: Vec<Vec<String>> = vec![];
        assert!(fuse(&empty, 5, DEFAULT_RRF_K).is_empty());
    }
}
