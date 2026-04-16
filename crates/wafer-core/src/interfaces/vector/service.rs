//! Vector storage and embedding generation interfaces.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorEntry {
    pub id: String,
    pub vector: Vec<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Required when the index has `keyword_search: true`; ignored otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorMatch {
    pub id: String,
    pub score: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DistanceMetric {
    Cosine,
    Euclidean,
    DotProduct,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Vector,
    Keyword,
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorIndexConfig {
    pub name: String,
    pub model: String,
    pub dimensions: u32,
    pub metric: DistanceMetric,
    #[serde(default)]
    pub keyword_search: bool,
}

/// Simple equality filter: each field must equal the given JSON value.
/// Nested paths use dot notation (`"user.id"` → matches `metadata.user.id`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MetadataFilter {
    #[serde(default)]
    pub equals: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Error, Debug)]
pub enum VectorError {
    #[error("vector index not found: {0}")]
    IndexNotFound(String),
    #[error("vector index already exists: {0}")]
    IndexAlreadyExists(String),
    #[error("keyword search is not enabled on this index")]
    KeywordSearchNotEnabled,
    #[error("dimension mismatch: index expects {expected}, got {got}")]
    DimensionMismatch { expected: u32, got: u32 },
    #[error("unknown embedding model: {0}")]
    UnknownModel(String),
    #[error("text required when index has keyword_search enabled")]
    TextRequired,
    #[error("keyword_query required for SearchMode::{0:?}")]
    KeywordQueryRequired(SearchMode),
    #[error("internal vector store error: {0}")]
    Internal(String),
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
}
