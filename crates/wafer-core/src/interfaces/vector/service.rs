//! Vector storage and embedding generation interfaces.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use wafer_block_macro::wafer_async_trait;

/// A single record stored in a vector index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorEntry {
    /// Caller-supplied id (must be unique within the index).
    pub id: String,
    /// Embedding vector for this entry.
    pub vector: Vec<f32>,
    /// Optional opaque metadata stored alongside the vector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Required when the index has `keyword_search: true`; ignored otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// One result row from a vector / keyword / hybrid query.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorMatch {
    /// Id of the matched entry.
    pub id: String,
    /// Score under the index's configured `DistanceMetric` (higher = better).
    pub score: f32,
    /// Metadata copied from the stored entry, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Distance metric used to score query matches.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DistanceMetric {
    /// Cosine similarity.
    Cosine,
    /// Negated Euclidean distance.
    Euclidean,
    /// Dot product.
    DotProduct,
}

/// Strategy used to rank query results.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    /// Pure vector similarity over the query embedding.
    Vector,
    /// Pure keyword search over the stored `text` field.
    Keyword,
    /// Vector + keyword fused via Reciprocal Rank Fusion.
    Hybrid,
}

/// Configuration for [`VectorService::create_index`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorIndexConfig {
    /// Index name (unique within the backend).
    pub name: String,
    /// Embedding model id this index expects (e.g. `bge-m3`).
    pub model: String,
    /// Embedding dimensionality the index will store.
    pub dimensions: u32,
    /// Distance metric used at query time.
    pub metric: DistanceMetric,
    /// When `true`, entries must include `text` and the backend maintains a keyword index over it.
    #[serde(default)]
    pub keyword_search: bool,
}

/// Simple equality filter: each field must equal the given JSON value.
/// Nested paths use dot notation (`"user.id"` → matches `metadata.user.id`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MetadataFilter {
    /// Equality conditions; all must match for the entry to be included.
    #[serde(default)]
    pub equals: std::collections::BTreeMap<String, serde_json::Value>,
}

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
    fn serialize_search_mode() {
        assert_eq!(
            serde_json::to_string(&SearchMode::Vector).unwrap(),
            "\"vector\""
        );
        assert_eq!(
            serde_json::to_string(&SearchMode::Keyword).unwrap(),
            "\"keyword\""
        );
        assert_eq!(
            serde_json::to_string(&SearchMode::Hybrid).unwrap(),
            "\"hybrid\""
        );
    }

    #[test]
    fn index_config_roundtrip() {
        let cfg = VectorIndexConfig {
            name: "docs".into(),
            model: "bge-m3".into(),
            dimensions: 1024,
            metric: DistanceMetric::Cosine,
            keyword_search: true,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: VectorIndexConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn keyword_search_defaults_false() {
        let json = r#"{"name":"i","model":"m","dimensions":1,"metric":"cosine"}"#;
        let cfg: VectorIndexConfig = serde_json::from_str(json).unwrap();
        assert!(!cfg.keyword_search);
    }

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
