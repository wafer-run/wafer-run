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

/// A single (id, vector, metadata) row to upsert into a vector index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorEntry {
    /// Caller-supplied row id.
    pub id: String,
    /// Embedding vector.
    pub vector: Vec<f32>,
    /// Arbitrary JSON metadata stored alongside the vector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Required when the index has `keyword_search: true`; ignored otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// One result row returned by `vector.query`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorMatch {
    /// Matched row id.
    pub id: String,
    /// Similarity score (metric-dependent).
    pub score: f32,
    /// Optional metadata payload (echoed from the stored entry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Distance metric used by a vector index.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DistanceMetric {
    /// Cosine similarity.
    Cosine,
    /// Euclidean (L2) distance.
    Euclidean,
    /// Dot product.
    DotProduct,
}

/// Search modality requested by `vector.query`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    /// Pure vector similarity search.
    Vector,
    /// Pure keyword/BM25 search.
    Keyword,
    /// Hybrid vector + keyword search.
    Hybrid,
}

/// Index configuration declared at creation time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorIndexConfig {
    /// Index name.
    pub name: String,
    /// Embedding model identifier used by this index.
    pub model: String,
    /// Vector dimensionality.
    pub dimensions: u32,
    /// Distance metric used for similarity.
    pub metric: DistanceMetric,
    /// Whether the index also stores text for keyword/hybrid search.
    #[serde(default)]
    pub keyword_search: bool,
}

/// Equality-only metadata filter. Keys are dot-paths into the entry metadata.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MetadataFilter {
    /// Equality constraints: each `path => value` must match exactly.
    #[serde(default)]
    pub equals: BTreeMap<String, serde_json::Value>,
}

// ---- Vector requests / responses ----

/// Request for `vector.create_index`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateIndexRequest {
    /// Index configuration to create.
    pub config: VectorIndexConfig,
}

/// Request for `vector.delete_index`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteIndexRequest {
    /// Name of the index to delete.
    pub name: String,
}

/// Request for `vector.upsert`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpsertRequest {
    /// Target index name.
    pub index: String,
    /// Entries to upsert.
    pub entries: Vec<VectorEntry>,
}

/// Request for `vector.query`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryRequest {
    /// Index name to query.
    pub index: String,
    /// Query vector (length must match the index `dimensions`).
    pub vector: Vec<f32>,
    /// Maximum number of results to return.
    pub top_k: usize,
    /// Optional metadata filter to apply.
    #[serde(default)]
    pub filter: Option<MetadataFilter>,
    /// Search modality (vector / keyword / hybrid).
    pub mode: SearchMode,
    /// Keyword query string (required when `mode` is `Keyword` or `Hybrid`).
    #[serde(default)]
    pub keyword_query: Option<String>,
}

/// Request for `vector.delete`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteRequest {
    /// Index name.
    pub index: String,
    /// Ids of rows to delete.
    pub ids: Vec<String>,
}

/// Request for `vector.count`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountRequest {
    /// Index name.
    pub index: String,
}

/// Response for `vector.query`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryResponse {
    /// Matched rows, sorted by descending similarity score.
    pub matches: Vec<VectorMatch>,
}

/// Response for `vector.count`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountResponse {
    /// Number of entries in the index.
    pub count: u64,
}

/// Request for `vector.list_indexes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListIndexesRequest {
    /// Namespace prefix to scan (e.g. `my_org__vector__`). Doubles as the
    /// WRAP authorization resource: it must parse to an owner via
    /// `resource_owner`, so partial prefixes fail closed.
    pub prefix: String,
}

/// Response for `vector.list_indexes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListIndexesResponse {
    /// Index stems (full storage names, prefix retained, `_meta` suffix
    /// stripped), in lexical order.
    pub indexes: Vec<String>,
}

/// Request for `vector.describe_index`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescribeIndexRequest {
    /// Index name (storage stem) to describe.
    pub index: String,
}

/// One column of an index's meta table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnInfo {
    /// Column name.
    pub name: String,
    /// Declared SQL type (e.g. `TEXT`, `INTEGER`).
    pub sql_type: String,
}

/// Response for `vector.describe_index`.
///
/// Absence is data, not an error: a missing index yields
/// `exists: false` with empty `columns` — callers use this op as an
/// existence/capability probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescribeIndexResponse {
    /// Whether the index's meta table exists.
    pub exists: bool,
    /// The meta table's actual columns in declaration order (real state —
    /// legacy indexes may drift from the canonical DDL).
    pub columns: Vec<ColumnInfo>,
    /// Whether the index has an FTS table (keyword search capability).
    pub keyword_search: bool,
}

/// Request for `vector.list_ids`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListIdsRequest {
    /// Index name.
    pub index: String,
    /// Metadata equality filter. Must contain at least one entry, and values
    /// must be JSON strings or numbers — enforced fail-closed by the service.
    pub filter: MetadataFilter,
}

/// Response for `vector.list_ids`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListIdsResponse {
    /// Ids of entries whose metadata matches every filter condition.
    pub ids: Vec<String>,
}

// ---- Embedding requests / responses ----

/// Request for `embedding.embed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedRequest {
    /// Texts to embed (one vector returned per input).
    pub texts: Vec<String>,
}

/// Response for `embedding.embed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbedResponse {
    /// Embedding model identifier that produced these vectors.
    pub model: String,
    /// Vector dimensionality.
    pub dimensions: u32,
    /// One vector per input text, in input order.
    pub vectors: Vec<Vec<f32>>,
}

/// Request for `embedding.count_tokens`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountTokensRequest {
    /// Text to tokenize.
    pub text: String,
}

/// Response for `embedding.count_tokens`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountTokensResponse {
    /// Number of tokens `text` decomposes to under the active embedding
    /// tokenizer.
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

    /// Representative query: metadata filter + hybrid mode + keyword query.
    /// Full decode equality pins the wire shape end-to-end.
    #[test]
    fn query_request_round_trips() {
        let mut equals = BTreeMap::new();
        equals.insert("user.id".to_string(), serde_json::json!("u1"));
        let original = QueryRequest {
            index: "docs".into(),
            vector: vec![1.0, 2.0],
            top_k: 5,
            filter: Some(MetadataFilter { equals }),
            mode: SearchMode::Hybrid,
            keyword_query: Some("cats".into()),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: QueryRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded, original);
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

    // -----------------------------------------------------------------------
    // JSON representation (config files / HTTP surfaces use serde_json)
    // -----------------------------------------------------------------------

    #[test]
    fn search_mode_serializes_lowercase_json() {
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
    fn index_config_json_round_trips() {
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
    fn list_indexes_request_round_trips() {
        let original = ListIndexesRequest {
            prefix: "my_org__vector__".into(),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ListIndexesRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn list_indexes_response_round_trips() {
        let original = ListIndexesResponse {
            indexes: vec!["my_org__vector__docs".into()],
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ListIndexesResponse = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn describe_index_response_round_trips() {
        let original = DescribeIndexResponse {
            exists: true,
            columns: vec![
                ColumnInfo {
                    name: "id".into(),
                    sql_type: "TEXT".into(),
                },
                ColumnInfo {
                    name: "metadata".into(),
                    sql_type: "TEXT".into(),
                },
            ],
            keyword_search: true,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: DescribeIndexResponse = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn list_ids_request_round_trips() {
        let mut filter = MetadataFilter::default();
        filter
            .equals
            .insert("document_id".into(), serde_json::json!("doc-1"));
        let original = ListIdsRequest {
            index: "my_org__vector__docs".into(),
            filter,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ListIdsRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn list_ids_response_round_trips() {
        let original = ListIdsResponse {
            ids: vec!["chunk-1".into(), "chunk-2".into()],
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ListIdsResponse = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded, original);
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
