//! Shared message handler logic for vector and embedding blocks.
//!
//! Any block implementing the `vector@v1` or `embedding@v1` interface can
//! delegate to these functions to avoid duplicating the message protocol
//! handling.

use serde::{Deserialize, Serialize};
use wafer_block::{
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    *,
};

use super::service::{
    EmbeddingService, MetadataFilter, SearchMode, VectorEntry, VectorError, VectorIndexConfig,
    VectorMatch, VectorService,
};

// --- Request types ---

#[derive(Deserialize)]
struct CreateIndexRequest {
    config: VectorIndexConfig,
}

#[derive(Deserialize)]
struct DeleteIndexRequest {
    name: String,
}

#[derive(Deserialize)]
struct UpsertRequest {
    index: String,
    entries: Vec<VectorEntry>,
}

#[derive(Deserialize)]
struct QueryRequest {
    index: String,
    vector: Vec<f32>,
    top_k: usize,
    #[serde(default)]
    filter: Option<MetadataFilter>,
    mode: SearchMode,
    #[serde(default)]
    keyword_query: Option<String>,
}

#[derive(Deserialize)]
struct DeleteRequest {
    index: String,
    ids: Vec<String>,
}

#[derive(Deserialize)]
struct CountRequest {
    index: String,
}

#[derive(Deserialize)]
struct EmbedRequest {
    texts: Vec<String>,
}

// --- Response types ---

#[derive(Serialize)]
struct QueryResponse {
    matches: Vec<VectorMatch>,
}

#[derive(Serialize)]
struct CountResponse {
    count: u64,
}

#[derive(Serialize)]
struct EmbedResponse {
    model: String,
    dimensions: u32,
    vectors: Vec<Vec<f32>>,
}

// --- Helpers ---

fn vector_error_to_wafer(e: VectorError) -> WaferError {
    match e {
        VectorError::IndexNotFound(_) => WaferError::new(ErrorCode::NOT_FOUND, e.to_string()),
        VectorError::IndexAlreadyExists(_) => {
            WaferError::new(ErrorCode::ALREADY_EXISTS, e.to_string())
        }
        VectorError::KeywordSearchNotEnabled
        | VectorError::DimensionMismatch { .. }
        | VectorError::UnknownModel(_)
        | VectorError::TextRequired
        | VectorError::KeywordQueryRequired(_) => {
            WaferError::new(ErrorCode::INVALID_ARGUMENT, e.to_string())
        }
        VectorError::Internal(msg) => {
            tracing::error!(error = %msg, "vector internal error");
            WaferError::new(ErrorCode::INTERNAL, "internal vector error")
        }
    }
}

use crate::interfaces::handler_util::{decode_or_err, to_output};

/// Handle a vector message using the given service.
pub async fn handle_message(
    service: &dyn VectorService,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::VECTOR_CREATE_INDEX => {
            let req = decode_or_err!(body, CreateIndexRequest, "vector.create_index");
            match service.create_index(req.config).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_DELETE_INDEX => {
            let req = decode_or_err!(body, DeleteIndexRequest, "vector.delete_index");
            match service.delete_index(&req.name).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_UPSERT => {
            let req = decode_or_err!(body, UpsertRequest, "vector.upsert");
            match service.upsert(&req.index, req.entries).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_QUERY => {
            let req = decode_or_err!(body, QueryRequest, "vector.query");
            match service
                .query(
                    &req.index,
                    req.vector,
                    req.top_k,
                    req.filter,
                    req.mode,
                    req.keyword_query,
                )
                .await
            {
                Ok(matches) => to_output(&QueryResponse { matches }),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_DELETE => {
            let req = decode_or_err!(body, DeleteRequest, "vector.delete");
            match service.delete(&req.index, req.ids).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_COUNT => {
            let req = decode_or_err!(body, CountRequest, "vector.count");
            match service.count(&req.index).await {
                Ok(count) => to_output(&CountResponse { count }),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown vector operation: {other}"),
        )),
    }
}

/// Handle an embedding message using the given service.
pub async fn handle_embedding_message(
    service: &dyn EmbeddingService,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::EMBEDDING_EMBED => {
            let req = decode_or_err!(body, EmbedRequest, "embedding.embed");
            match service.embed(req.texts).await {
                Ok(vectors) => to_output(&EmbedResponse {
                    model: service.model().to_string(),
                    dimensions: service.dimensions(),
                    vectors,
                }),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown embedding operation: {other}"),
        )),
    }
}
