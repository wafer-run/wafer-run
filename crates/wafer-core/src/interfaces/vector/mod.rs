//! Vector search and embedding interfaces.

/// Built-in model catalog mapping known embedding model ids to metadata.
pub mod catalog;
/// Shared message handler that routes `vector.*` and `embedding.*` ops.
pub mod handler;
/// Reciprocal Rank Fusion helpers used to merge vector and keyword result sets.
pub mod rrf;
/// `VectorService` and `EmbeddingService` traits plus their data types.
pub mod service;

pub use catalog::{get_model, model_catalog, ModelInfo, RuntimeCompat, DEFAULT_MODEL};
pub use rrf::{fuse, fuse_scored, DEFAULT_RRF_K};
pub use service::{
    DistanceMetric, EmbeddingService, MetadataFilter, Result as VectorResult, SearchMode,
    VectorEntry, VectorError, VectorIndexConfig, VectorMatch, VectorService,
};

#[cfg(test)]
mod reexport_tests {
    //! Pins what this module re-exports, reached through the module path a
    //! consumer writes rather than through `rrf::`.

    use super::{fuse, fuse_scored, DEFAULT_RRF_K};

    /// `fuse` and `fuse_scored` are the same fusion; the difference is only
    /// that `fuse` throws the relevance number away. A consumer that needs
    /// the score therefore has no reason to re-implement RRF — which is
    /// what the missing re-export used to invite.
    #[test]
    fn fuse_scored_agrees_with_fuse_and_keeps_the_scores() {
        let a: Vec<String> = ["x", "y", "z"].iter().map(|s| s.to_string()).collect();
        let b: Vec<String> = ["y", "x"].iter().map(|s| s.to_string()).collect();

        let scored = fuse_scored(&[a.clone(), b.clone()], 10, DEFAULT_RRF_K);
        let plain = fuse(&[a, b], 10, DEFAULT_RRF_K);

        assert_eq!(
            scored.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
            plain,
            "the two entry points must rank identically"
        );
        assert!(
            scored.iter().all(|(_, score)| *score > 0.0),
            "fuse_scored must carry the real RRF value, not a placeholder: {scored:?}"
        );
    }
}
