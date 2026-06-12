//! Shared message handler logic for vector and embedding blocks.
//!
//! Any block implementing the `vector@v1` or `embedding@v1` interface can
//! delegate to these functions to avoid duplicating the message protocol
//! handling.
//!
//! The service traits operate directly on the `wafer_block::wire::vector`
//! types (re-exported from `super::service`), so request/response values pass
//! straight through — only `VectorError` needs mapping onto wire error codes.

use wafer_block::{
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    wire::vector as wire,
    *,
};

use super::service::{EmbeddingService, VectorError, VectorService};
use crate::interfaces::handler_util::{decode_or_err, to_output};

// --- Helpers ---

fn vector_error_to_wafer(e: VectorError) -> WaferError {
    match e {
        VectorError::IndexNotFound(_) => WaferError::new(ErrorCode::NotFound, e.to_string()),
        VectorError::IndexAlreadyExists(_) => {
            WaferError::new(ErrorCode::AlreadyExists, e.to_string())
        }
        VectorError::KeywordSearchNotEnabled
        | VectorError::DimensionMismatch { .. }
        | VectorError::UnknownModel(_)
        | VectorError::TextRequired
        | VectorError::KeywordQueryRequired(_)
        | VectorError::InvalidIndexName(_) => {
            WaferError::new(ErrorCode::InvalidArgument, e.to_string())
        }
        VectorError::Internal(msg) => {
            tracing::error!(error = %msg, "vector internal error");
            WaferError::new(ErrorCode::Internal, "internal vector error")
        }
    }
}

/// Handle a vector message using the given service.
pub async fn handle_message(
    service: &dyn VectorService,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::VECTOR_CREATE_INDEX => {
            let req = decode_or_err!(body, wire::CreateIndexRequest, "vector.create_index");
            match service.create_index(req.config).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_DELETE_INDEX => {
            let req = decode_or_err!(body, wire::DeleteIndexRequest, "vector.delete_index");
            match service.delete_index(&req.name).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_UPSERT => {
            let req = decode_or_err!(body, wire::UpsertRequest, "vector.upsert");
            match service.upsert(&req.index, req.entries).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_QUERY => {
            let req = decode_or_err!(body, wire::QueryRequest, "vector.query");
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
                Ok(matches) => to_output(&wire::QueryResponse { matches }),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_DELETE => {
            let req = decode_or_err!(body, wire::DeleteRequest, "vector.delete");
            match service.delete(&req.index, req.ids).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_COUNT => {
            let req = decode_or_err!(body, wire::CountRequest, "vector.count");
            match service.count(&req.index).await {
                Ok(count) => to_output(&wire::CountResponse { count }),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::Unimplemented,
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
            let req = decode_or_err!(body, wire::EmbedRequest, "embedding.embed");
            match service.embed(req.texts).await {
                Ok(vectors) => to_output(&wire::EmbedResponse {
                    model: service.model().to_string(),
                    dimensions: service.dimensions(),
                    vectors,
                }),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::EMBEDDING_COUNT_TOKENS => {
            let req = decode_or_err!(body, wire::CountTokensRequest, "embedding.count_tokens");
            to_output(&wire::CountTokensResponse {
                tokens: service.count_tokens(&req.text) as u64,
            })
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("unknown embedding operation: {other}"),
        )),
    }
}
