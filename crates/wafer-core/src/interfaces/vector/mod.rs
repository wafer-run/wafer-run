//! Vector search and embedding interfaces.

pub mod service;

pub use service::{
    DistanceMetric, MetadataFilter, SearchMode, VectorEntry, VectorError, VectorIndexConfig,
    VectorMatch,
};
