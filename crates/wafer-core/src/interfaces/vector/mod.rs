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
pub use rrf::{fuse, DEFAULT_RRF_K};
pub use service::{
    DistanceMetric, EmbeddingService, MetadataFilter, Result as VectorResult, SearchMode,
    VectorEntry, VectorError, VectorIndexConfig, VectorMatch, VectorService,
};
