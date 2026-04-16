//! Vector search and embedding interfaces.

pub mod catalog;
pub mod service;

pub use catalog::{get_model, model_catalog, ModelInfo, RuntimeCompat, DEFAULT_MODEL};
pub use service::{
    DistanceMetric, MetadataFilter, SearchMode, VectorEntry, VectorError, VectorIndexConfig,
    VectorMatch,
};
