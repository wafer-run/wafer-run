//! Reciprocal Rank Fusion for combining ranked result lists.
//!
//! Given N ranked lists of document IDs, compute
//! `score(d) = sum over lists of 1 / (k + rank_in_list(d))`
//! for each document that appears in any list, then sort descending.

use std::collections::BTreeMap;

/// Default RRF dampening constant (`k`) recommended by Cormack et al. (2009).
pub const DEFAULT_RRF_K: f32 = 60.0;

/// Fuse multiple ranked ID lists into a single ranked list via RRF, keeping
/// each surviving document's fused RRF score.
///
/// `rankings[i]` is an ordered list of document IDs (best first).
/// Returns up to `top_k` `(id, score)` pairs sorted by fused score descending.
/// The score is the real RRF value (`sum of 1 / (k + rank)`), so callers can
/// expose a meaningful relevance number rather than a positional placeholder.
pub fn fuse_scored(rankings: &[Vec<String>], top_k: usize, k: f32) -> Vec<(String, f32)> {
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
    pairs.truncate(top_k);
    pairs
}

/// Fuse multiple ranked ID lists into a single ranked list via RRF.
///
/// Convenience wrapper over [`fuse_scored`] that discards the scores.
/// Returns up to `top_k` IDs sorted by fused score descending.
pub fn fuse(rankings: &[Vec<String>], top_k: usize, k: f32) -> Vec<String> {
    fuse_scored(rankings, top_k, k)
        .into_iter()
        .map(|(id, _)| id)
        .collect()
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

    #[test]
    fn fuse_scored_returns_real_rrf_scores_not_positional() {
        // A document appearing in both lists must score higher than the sum of
        // its two reciprocal-rank contributions, and the scores must be the
        // real RRF values — small fractions like ~1/61, NOT the positional
        // 1.0, 0.5, 0.333… placeholder the caller used to fabricate.
        let a = ids(&["x", "y", "z"]);
        let b = ids(&["y", "w", "x"]);
        let scored = fuse_scored(&[a, b], 10, DEFAULT_RRF_K);
        // y: 1/(60+2) + 1/(60+1) ≈ 0.0327; x: 1/(60+1) + 1/(60+3) ≈ 0.0323
        assert_eq!(scored[0].0, "y");
        assert_eq!(scored[1].0, "x");
        assert!(
            (scored[0].1 - (1.0 / 62.0 + 1.0 / 61.0)).abs() < 1e-6,
            "top score should be the real fused RRF value, got {}",
            scored[0].1
        );
        // Scores strictly decrease and are nowhere near the old 1.0 placeholder.
        assert!(scored[0].1 > scored[1].1);
        assert!(scored[0].1 < 0.1, "RRF scores are small fractions");
    }

    #[test]
    fn fuse_scored_respects_top_k() {
        let a = ids(&["a", "b", "c", "d", "e"]);
        let scored = fuse_scored(&[a], 3, DEFAULT_RRF_K);
        assert_eq!(scored.len(), 3);
        assert_eq!(scored[0].0, "a");
    }
}
