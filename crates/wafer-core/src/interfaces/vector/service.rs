//! Vector storage and embedding generation interfaces.
//!
//! The data types (`VectorEntry`, `VectorIndexConfig`, `SearchMode`, …) are
//! the canonical wire types from `wafer_block::wire::vector`, re-exported
//! here so service impls and wire-level consumers share one definition —
//! there is no separate service-side representation and no conversion layer.
//! Only the genuinely service-side items (`VectorService`,
//! `EmbeddingService`, `VectorError`) live in this module.

use thiserror::Error;
pub use wafer_block::wire::vector::{
    is_legacy_spelling_of, ColumnInfo, DescribeIndexResponse, DistanceMetric, MetadataFilter,
    SearchMode, VectorEntry, VectorIndexConfig, VectorMatch,
};
use wafer_block_macro::wafer_async_trait;

/// Errors returned by [`VectorService`] and [`EmbeddingService`] operations.
#[derive(Error, Debug)]
pub enum VectorError {
    /// No index exists with the requested name.
    #[error("vector index not found: {0}")]
    IndexNotFound(String),
    /// `create_index` called with a name that already exists, or
    /// `rename_index` called with a target that already exists.
    #[error("vector index already exists: {0}")]
    IndexAlreadyExists(String),
    /// Index name is not a plain identifier
    /// ([`wafer_block::db::is_plain_ident`]: 1 to 63 of lowercase ASCII
    /// letters, digits and `_`).
    ///
    /// Index names become SQL table names in the SQLite backend, so a
    /// non-identifier name is rejected fail-closed rather than silently
    /// rewritten into a different valid name.
    #[error(
        "invalid vector index name: {0:?} (1 to 63 of: lowercase ASCII letters, digits, underscore)"
    )]
    InvalidIndexName(String),
    /// `rename_index` called with a `from` that is not a legacy spelling of
    /// `to` ([`is_legacy_spelling_of`]): the op only moves a mixed-case
    /// index name to its lowercase spelling.
    #[error(
        "cannot rename vector index {from:?} to {to:?}: `to` must be a valid index name and `from` the same name with some letters uppercase"
    )]
    InvalidRename {
        /// The rejected source name.
        from: String,
        /// The rejected target name.
        to: String,
    },
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
    /// `list_ids` called with an unusable metadata filter (no conditions, or
    /// a non-scalar value). Rejected fail-closed: an unconditioned id dump
    /// is not a supported query shape, and bind-equality against
    /// `json_extract` output is ill-defined for non-scalar JSON values.
    #[error("invalid metadata filter: {0}")]
    InvalidMetadataFilter(String),
    /// Backend-internal failure.
    #[error("internal vector store error: {0}")]
    Internal(String),
}

/// Convenience alias for `Result` types returned by the vector interfaces.
pub type Result<T> = std::result::Result<T, VectorError>;

/// Check the names of a [`VectorService::rename_index`] call: `from` must be
/// a legacy spelling of `to` ([`is_legacy_spelling_of`]), or the call is
/// [`VectorError::InvalidRename`]. Every backend runs this before touching
/// storage, and the vector handler runs it before authorizing, so the rule
/// is the same wherever the op is served.
pub fn check_rename(from: &str, to: &str) -> Result<()> {
    if is_legacy_spelling_of(from, to) {
        Ok(())
    } else {
        Err(VectorError::InvalidRename {
            from: from.to_string(),
            to: to.to_string(),
        })
    }
}

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
    /// Move the index `from` to `to`, entries, metadata and keyword search
    /// included, so an index created before index names had to be lowercase
    /// can be reached again.
    ///
    /// The names must pass [`check_rename`]: `to` is a plain index name and
    /// `from` is the same name with some letters uppercase. This is the only
    /// op that accepts such a `from`; it is matched exactly, never
    /// case-insensitively. Errors:
    /// - [`VectorError::InvalidRename`] when the names fail [`check_rename`];
    /// - [`VectorError::IndexNotFound`] (naming `from`) when no index is
    ///   stored under exactly `from`. A caller migrating at startup treats
    ///   this as already done when `to` exists;
    /// - [`VectorError::IndexAlreadyExists`] (naming `to`) when `to`, or any
    ///   storage it would occupy, already exists. Two spellings of one name
    ///   are never merged.
    ///
    /// The move is atomic: on any error nothing has changed.
    async fn rename_index(&self, from: &str, to: &str) -> Result<()>;
    /// List the index stems (storage names, `_meta` suffix stripped) whose
    /// meta tables live under `prefix`, in lexical order. The prefix matches
    /// literally — `_`/`%` in it are not LIKE wildcards.
    ///
    /// Defaulted to `Internal` so existing backends keep compiling; backends
    /// that can enumerate their catalog should override.
    async fn list_indexes(&self, prefix: &str) -> Result<Vec<String>> {
        let _ = prefix;
        Err(VectorError::Internal(
            "list_indexes not implemented by this backend".into(),
        ))
    }
    /// Describe `index`: existence, meta-table columns (declaration order),
    /// and keyword-search capability. Absence is data (`exists: false`), not
    /// an error — callers use this as an existence/capability probe.
    ///
    /// Defaulted to `Internal` so existing backends keep compiling.
    async fn describe_index(&self, index: &str) -> Result<DescribeIndexResponse> {
        let _ = index;
        Err(VectorError::Internal(
            "describe_index not implemented by this backend".into(),
        ))
    }
    /// Ids of entries in `index` whose metadata satisfies every
    /// `filter.equals` condition. The filter must be non-empty and its
    /// values JSON strings or numbers ([`VectorError::InvalidMetadataFilter`]
    /// otherwise); a missing index is [`VectorError::IndexNotFound`].
    ///
    /// Defaulted to `Internal` so existing backends keep compiling.
    async fn list_ids(&self, index: &str, filter: MetadataFilter) -> Result<Vec<String>> {
        let _ = (index, filter);
        Err(VectorError::Internal(
            "list_ids not implemented by this backend".into(),
        ))
    }
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
