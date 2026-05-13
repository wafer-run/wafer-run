//! Wire-format types for the vector and embedding services.
//!
//! Mirrors `crates/wafer-core/src/interfaces/vector/handler.rs` and
//! `crates/wafer-core/src/interfaces/vector/service.rs`. Embeddings are
//! `Vec<f32>` — MessagePack encodes f32 as 5-byte values (`0xca` tag + 4
//! bytes), so a 1024-dim vector is ~5 KiB on the wire. The no-inflation test
//! locks this at the wire-types level.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// ---- Index / entry types ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorEntry {
    pub id: String,
    pub vector: Vec<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Required when the index has `keyword_search: true`; ignored otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorIndexConfig {
    pub name: String,
    pub model: String,
    pub dimensions: u32,
    pub metric: DistanceMetric,
    #[serde(default)]
    pub keyword_search: bool,
}

/// Equality-only metadata filter. Keys are dot-paths into the entry metadata.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MetadataFilter {
    #[serde(default)]
    pub equals: BTreeMap<String, serde_json::Value>,
}

// ---- Vector requests / responses ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateIndexRequest {
    pub config: VectorIndexConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteIndexRequest {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpsertRequest {
    pub index: String,
    pub entries: Vec<VectorEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryRequest {
    pub index: String,
    pub vector: Vec<f32>,
    pub top_k: usize,
    #[serde(default)]
    pub filter: Option<MetadataFilter>,
    pub mode: SearchMode,
    #[serde(default)]
    pub keyword_query: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteRequest {
    pub index: String,
    pub ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountRequest {
    pub index: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryResponse {
    pub matches: Vec<VectorMatch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountResponse {
    pub count: u64,
}

// ---- Embedding requests / responses ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedRequest {
    pub texts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbedResponse {
    pub model: String,
    pub dimensions: u32,
    pub vectors: Vec<Vec<f32>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountTokensRequest {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountTokensResponse {
    pub tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    // -----------------------------------------------------------------------
    // Round-trip tests
    // -----------------------------------------------------------------------

    #[test]
    fn create_index_request_round_trips() {
        let original = CreateIndexRequest {
            config: VectorIndexConfig {
                name: "docs".into(),
                model: "bge-m3".into(),
                dimensions: 1024,
                metric: DistanceMetric::Cosine,
                keyword_search: true,
            },
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: CreateIndexRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.config.name, "docs");
        assert_eq!(decoded.config.dimensions, 1024);
        assert!(decoded.config.keyword_search);
    }

    #[test]
    fn upsert_request_round_trips() {
        let original = UpsertRequest {
            index: "docs".into(),
            entries: vec![VectorEntry {
                id: "e1".into(),
                vector: vec![0.1, 0.2, 0.3],
                metadata: Some(serde_json::json!({"k": "v"})),
                text: Some("hello".into()),
            }],
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: UpsertRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.entries.len(), 1);
        assert_eq!(decoded.entries[0].id, "e1");
        assert_eq!(decoded.entries[0].vector.len(), 3);
        assert_eq!(decoded.entries[0].text.as_deref(), Some("hello"));
    }

    #[test]
    fn query_request_round_trips() {
        let original = QueryRequest {
            index: "docs".into(),
            vector: vec![1.0, 2.0],
            top_k: 5,
            filter: None,
            mode: SearchMode::Vector,
            keyword_query: None,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: QueryRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.top_k, 5);
        assert_eq!(decoded.mode, SearchMode::Vector);
        assert_eq!(decoded.vector.len(), 2);
    }

    #[test]
    fn query_response_round_trips() {
        let original = QueryResponse {
            matches: vec![VectorMatch {
                id: "e1".into(),
                score: 0.9,
                metadata: None,
            }],
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: QueryResponse = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.matches.len(), 1);
        assert_eq!(decoded.matches[0].id, "e1");
    }

    #[test]
    fn embed_response_round_trips() {
        let original = EmbedResponse {
            model: "bge-m3".into(),
            dimensions: 3,
            vectors: vec![vec![0.1, 0.2, 0.3]],
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: EmbedResponse = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.model, "bge-m3");
        assert_eq!(decoded.dimensions, 3);
        assert_eq!(decoded.vectors.len(), 1);
        assert_eq!(decoded.vectors[0].len(), 3);
    }

    // -----------------------------------------------------------------------
    // No-inflation tests (Vec<f32> embeddings must not balloon encoded size)
    // -----------------------------------------------------------------------

    /// 256 KiB of f32 (= 65 536 elements) round-trips at ~5 bytes per float
    /// (`0xca` tag + 4 IEEE-754 bytes). Locks the per-element invariant.
    #[test]
    fn query_request_vector_no_inflation() {
        let n = 65_536;
        let req = QueryRequest {
            index: "i".into(),
            vector: vec![0.0f32; n],
            top_k: 1,
            filter: None,
            mode: SearchMode::Vector,
            keyword_query: None,
        };
        let encoded = codec::encode(&req).expect("encode");
        // Each f32 is encoded as 5 bytes (0xca tag + 4 bytes).
        // 65 536 * 5 = 327 680 bytes payload, plus small framing.
        let max = n * 5 + 1024;
        assert!(
            encoded.len() < max,
            "QueryRequest.vector inflated to {} bytes — should be ~{} bytes ({}-element f32 array)",
            encoded.len(),
            max,
            n
        );
    }

    #[test]
    fn embed_response_vectors_no_inflation() {
        // 16 vectors of 1024 f32 each (~64 KiB of float payload).
        let dim = 1024usize;
        let n = 16usize;
        let resp = EmbedResponse {
            model: "m".into(),
            dimensions: dim as u32,
            vectors: vec![vec![0.0f32; dim]; n],
        };
        let encoded = codec::encode(&resp).expect("encode");
        let max = n * dim * 5 + 1024;
        assert!(
            encoded.len() < max,
            "EmbedResponse.vectors inflated to {} bytes — should be ~{} bytes",
            encoded.len(),
            max
        );
    }

    // -----------------------------------------------------------------------
    // Schema-lock tests
    // -----------------------------------------------------------------------

    #[test]
    fn schema_lock_query_request() {
        let req = QueryRequest {
            index: String::new(),
            vector: vec![],
            top_k: 0,
            filter: None,
            mode: SearchMode::Vector,
            keyword_query: None,
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "86a5696e646578a0a6766563746f7290a5746f705f6b00a666696c746572c0a46d6f6465a6766563746f72ad6b6579776f72645f7175657279c0",
            "QueryRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_create_index_request() {
        let req = CreateIndexRequest {
            config: VectorIndexConfig {
                name: String::new(),
                model: String::new(),
                dimensions: 0,
                metric: DistanceMetric::Cosine,
                keyword_search: false,
            },
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a6636f6e66696785a46e616d65a0a56d6f64656ca0aa64696d656e73696f6e7300a66d6574726963a6636f73696e65ae6b6579776f72645f736561726368c2",
            "CreateIndexRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_query_response() {
        let resp = QueryResponse { matches: vec![] };
        let encoded = codec::encode(&resp).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a76d61746368657390",
            "QueryResponse schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_count_response() {
        let resp = CountResponse { count: 0 };
        let encoded = codec::encode(&resp).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a5636f756e7400",
            "CountResponse schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_embed_request() {
        let req = EmbedRequest { texts: vec![] };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a5746578747390",
            "EmbedRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_embed_response() {
        let resp = EmbedResponse {
            model: String::new(),
            dimensions: 0,
            vectors: vec![],
        };
        let encoded = codec::encode(&resp).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "83a56d6f64656ca0aa64696d656e73696f6e7300a7766563746f727390",
            "EmbedResponse schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn count_tokens_request_round_trips() {
        let original = CountTokensRequest {
            text: "hello world".into(),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: CountTokensRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.text, "hello world");
    }

    #[test]
    fn count_tokens_response_round_trips() {
        let original = CountTokensResponse { tokens: 42 };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: CountTokensResponse = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.tokens, 42);
    }

    #[test]
    fn schema_lock_count_tokens_request() {
        let req = CountTokensRequest {
            text: String::new(),
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a474657874a0",
            "CountTokensRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_count_tokens_response() {
        let resp = CountTokensResponse { tokens: 0 };
        let encoded = codec::encode(&resp).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a6746f6b656e7300",
            "CountTokensResponse schema changed — review consumer impact before updating this literal"
        );
    }
}
