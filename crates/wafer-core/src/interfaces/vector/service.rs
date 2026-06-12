//! Vector storage and embedding generation interfaces.
//!
//! The data types (`VectorEntry`, `VectorIndexConfig`, `SearchMode`, …) are
//! the canonical wire types from `wafer_block::wire::vector`, re-exported
//! here so service impls and wire-level consumers share one definition —
//! there is no separate service-side representation and no conversion layer.
//! Only the genuinely service-side items (`VectorService`,
//! `EmbeddingService`, `VectorError`) live in this module.

use thiserror::Error;
use wafer_block_macro::wafer_async_trait;

pub use wafer_block::wire::vector::{
    DistanceMetric, MetadataFilter, SearchMode, VectorEntry, VectorIndexConfig, VectorMatch,
};

/// Errors returned by [`VectorService`] and [`EmbeddingService`] operations.
#[derive(Error, Debug)]
pub enum VectorError {
    /// No index exists with the requested name.
    #[error("vector index not found: {0}")]
    IndexNotFound(String),
    /// `create_index` called with a name that already exists.
    #[error("vector index already exists: {0}")]
    IndexAlreadyExists(String),
    /// Index name is not a plain identifier (`[A-Za-z0-9_]`, non-empty).
    ///
    /// Index names become SQL table names in the SQLite backend, so a
    /// non-identifier name is rejected fail-closed rather than silently
    /// rewritten into a different valid name.
    #[error(
        "invalid vector index name: {0:?} (only ASCII alphanumerics and underscore are allowed)"
    )]
    InvalidIndexName(String),
    /// Caller requested keyword / hybrid search on a vector-only index.
    #[error("keyword search is not enabled on this index")]
    KeywordSearchNotEnabled,
    /// Vector length did not match the index's configured dimensionality.
    #[error("dimension mismatch: index expects {expected}, got {got}")]
    DimensionMismatch {
        /// Dimensionality the index was created with.
        expected: u32,
        /// Dimensionality of the rejected vector.
        got: u32,
    },
    /// Embedding model id is not known to the backend.
    #[error("unknown embedding model: {0}")]
    UnknownModel(String),
    /// `VectorEntry.text` was missing on an index that requires it.
    #[error("text required when index has keyword_search enabled")]
    TextRequired,
    /// `query` called in keyword / hybrid mode without supplying `keyword_query`.
    #[error("keyword_query required for SearchMode::{0:?}")]
    KeywordQueryRequired(SearchMode),
    /// Backend-internal failure.
    #[error("internal vector store error: {0}")]
    Internal(String),
}

/// Convenience alias for `Result` types returned by the vector interfaces.
pub type Result<T> = std::result::Result<T, VectorError>;

/// Vector store interface — create/destroy indexes, upsert entries, query/delete by id.
#[wafer_async_trait]
pub trait VectorService: wafer_block::MaybeSend + wafer_block::MaybeSync {
    /// Create a new index described by `config`.
    async fn create_index(&self, config: VectorIndexConfig) -> Result<()>;
    /// Drop the index `name` and all of its entries.
    async fn delete_index(&self, name: &str) -> Result<()>;
    /// Insert-or-replace `entries` in `index`.
    async fn upsert(&self, index: &str, entries: Vec<VectorEntry>) -> Result<()>;
    /// Return the top-`top_k` matches in `index` for `vector` under the given `mode`.
    async fn query(
        &self,
        index: &str,
        vector: Vec<f32>,
        top_k: usize,
        filter: Option<MetadataFilter>,
        mode: SearchMode,
        keyword_query: Option<String>,
    ) -> Result<Vec<VectorMatch>>;
    /// Remove the entries whose ids are in `ids` from `index`.
    async fn delete(&self, index: &str, ids: Vec<String>) -> Result<()>;
    /// Return the number of entries currently stored in `index`.
    async fn count(&self, index: &str) -> Result<u64>;
}

/// Embedding model interface — convert text into fixed-dimensional vectors.
#[wafer_async_trait]
pub trait EmbeddingService: wafer_block::MaybeSend + wafer_block::MaybeSync {
    /// Identifier of the underlying embedding model.
    fn model(&self) -> &str;
    /// Output dimensionality of the underlying embedding model.
    fn dimensions(&self) -> u32;
    /// Embed `texts` and return one vector per input.
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>>;

    /// Count the number of model-native tokens in `text`.
    ///
    /// Used by the vector block's chunker to size chunks accurately for
    /// multilingual content where whitespace-word count diverges from BPE
    /// token count (CJK, heavy punctuation, agglutinative languages).
    ///
    /// Default impl returns the whitespace-word count — a usable proxy for
    /// English prose at bge-m3 chunk granularity. Implementations backed by
    /// a real tokenizer should override.
    fn count_tokens(&self, text: &str) -> usize {
        text.split_whitespace().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_count_tokens_is_whitespace_split() {
        struct Mock;
        #[wafer_async_trait]
        impl EmbeddingService for Mock {
            fn model(&self) -> &str {
                "mock"
            }
            fn dimensions(&self) -> u32 {
                0
            }
            async fn embed(&self, _texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
                Ok(Vec::new())
            }
        }
        let m = Mock;
        assert_eq!(m.count_tokens(""), 0);
        assert_eq!(m.count_tokens("hello world"), 2);
        assert_eq!(m.count_tokens("  spaced   out  text  "), 3);
    }
}
