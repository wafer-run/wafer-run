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
    context::Context,
    streams::output::OutputStream,
    types::ResourceType,
    wire::vector as wire,
    *,
};

use super::service::{EmbeddingService, VectorError, VectorService};
use crate::interfaces::handler_util::{decode_and_authorize, decode_or_err, to_output};

/// The three read-only vector ops authorize with `is_write = false`; every
/// other op mutates the index and authorizes with `is_write = true`.
const READ: bool = false;
const WRITE: bool = true;

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
///
/// Each op is WRAP-authorized host-side against its decoded index name
/// (`ResourceType::Vector`) before reaching the service — a caller can only
/// touch indexes in its own `{org}__{block}__*` namespace.
pub async fn handle_message(
    service: &dyn VectorService,
    ctx: &dyn Context,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::VECTOR_CREATE_INDEX => {
            let req = match decode_and_authorize::<wire::CreateIndexRequest>(
                ctx,
                body,
                "vector.create_index",
                |r| (r.config.name.clone(), ResourceType::Vector, WRITE),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.create_index(req.config).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_DELETE_INDEX => {
            let req = match decode_and_authorize::<wire::DeleteIndexRequest>(
                ctx,
                body,
                "vector.delete_index",
                |r| (r.name.clone(), ResourceType::Vector, WRITE),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.delete_index(&req.name).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_UPSERT => {
            let req =
                match decode_and_authorize::<wire::UpsertRequest>(ctx, body, "vector.upsert", |r| {
                    (r.index.clone(), ResourceType::Vector, WRITE)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            match service.upsert(&req.index, req.entries).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_QUERY => {
            let req =
                match decode_and_authorize::<wire::QueryRequest>(ctx, body, "vector.query", |r| {
                    (r.index.clone(), ResourceType::Vector, READ)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
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
            let req =
                match decode_and_authorize::<wire::DeleteRequest>(ctx, body, "vector.delete", |r| {
                    (r.index.clone(), ResourceType::Vector, WRITE)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
            match service.delete(&req.index, req.ids).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_COUNT => {
            let req =
                match decode_and_authorize::<wire::CountRequest>(ctx, body, "vector.count", |r| {
                    (r.index.clone(), ResourceType::Vector, READ)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };
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
