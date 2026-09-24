//! Shared message handler logic for vector and embedding blocks. Both
//! authorize every op host-side before the service runs (see
//! [`handle_message`] and [`handle_embedding_message`]).
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
    wrap::op_resource,
    *,
};

use super::service::{check_rename, EmbeddingService, VectorError, VectorService};
use crate::interfaces::handler_util::{decode_and_authorize, decode_and_authorize_all, to_output};

/// The read-only vector ops (query, count, list_indexes, describe_index,
/// list_ids) and both embedding ops authorize for `ResourceAccess::Read`;
/// every other vector op mutates the index and authorizes for
/// `ResourceAccess::Write`.
const READ: ResourceAccess = ResourceAccess::Read;
const WRITE: ResourceAccess = ResourceAccess::Write;

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
        | VectorError::InvalidIndexName(_)
        | VectorError::InvalidRename { .. }
        | VectorError::InvalidMetadataFilter(_) => {
            WaferError::new(ErrorCode::InvalidArgument, e.to_string())
        }
        // Transient: the code tells a caller it may retry. The driver's
        // message can name files and hosts, so it is logged, not returned.
        VectorError::Unavailable(msg) => {
            tracing::warn!(error = %msg, "vector store temporarily unavailable");
            WaferError::new(
                ErrorCode::Unavailable,
                "vector store temporarily unavailable",
            )
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
        ServiceOp::VECTOR_LIST_INDEXES => {
            // Authorizes on the literal prefix: `resource_owner` requires a
            // full `{org}__{block}__` namespace, so partial prefixes are
            // unnamespaced and deny under Rule 7.
            let req = match decode_and_authorize::<wire::ListIndexesRequest>(
                ctx,
                body,
                "vector.list_indexes",
                |r| (r.prefix.clone(), ResourceType::Vector, READ),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.list_indexes(&req.prefix).await {
                Ok(indexes) => to_output(&wire::ListIndexesResponse { indexes }),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_DESCRIBE_INDEX => {
            let req = match decode_and_authorize::<wire::DescribeIndexRequest>(
                ctx,
                body,
                "vector.describe_index",
                |r| (r.index.clone(), ResourceType::Vector, READ),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.describe_index(&req.index).await {
                Ok(desc) => to_output(&desc),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_LIST_IDS => {
            let req = match decode_and_authorize::<wire::ListIdsRequest>(
                ctx,
                body,
                "vector.list_ids",
                |r| (r.index.clone(), ResourceType::Vector, READ),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.list_ids(&req.index, req.filter).await {
                Ok(ids) => to_output(&wire::ListIdsResponse { ids }),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_RENAME_INDEX => {
            // Both names are authorized for write: the op empties `from` and
            // fills `to`. Names that fail the rename rule are refused before
            // either check, as malformed whatever the caller's grants.
            let req = match decode_and_authorize_all::<wire::RenameIndexRequest>(
                ctx,
                body,
                "vector.rename_index",
                |r| {
                    check_rename(&r.from, &r.to).map_err(vector_error_to_wafer)?;
                    Ok(vec![
                        (r.from.clone(), ResourceType::Vector, WRITE),
                        (r.to.clone(), ResourceType::Vector, WRITE),
                    ])
                },
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.rename_index(&req.from, &req.to).await {
                Ok(()) => OutputStream::respond(vec![]),
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
///
/// `block` is the registered name of the block serving the call, and `ctx`
/// its context for this call. Each op authorizes `ctx.caller_id()` for
/// `Read` through `ctx.check_resource_access` against its
/// [`ResourceType::Embedding`] resource in `block`'s namespace
/// ([`op_resource`]: `{org}__{block}__embed`, `{org}__{block}__count_tokens`)
/// before the service is touched. The serving block and the admin block are
/// admitted; any other caller needs a grant the serving block declares (see
/// [`EmbeddingService::grants`]).
pub async fn handle_embedding_message(
    service: &dyn EmbeddingService,
    ctx: &dyn Context,
    block: &str,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    let op = msg.kind.as_str();
    match op {
        ServiceOp::EMBEDDING_EMBED => {
            let req = match decode_and_authorize::<wire::EmbedRequest>(
                ctx,
                body,
                "embedding.embed",
                |_| (op_resource(block, op), ResourceType::Embedding, READ),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
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
            let req = match decode_and_authorize::<wire::CountTokensRequest>(
                ctx,
                body,
                "embedding.count_tokens",
                |_| (op_resource(block, op), ResourceType::Embedding, READ),
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
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

#[cfg(test)]
mod tests {
    use wafer_block::ErrorCode;

    use super::{vector_error_to_wafer, VectorError};

    #[test]
    fn unavailable_is_transient_and_scrubbed() {
        let w = vector_error_to_wafer(VectorError::Unavailable(
            "database is locked: /srv/data/app.db".into(),
        ));
        assert_eq!(w.code, ErrorCode::Unavailable);
        assert_eq!(w.message, "vector store temporarily unavailable");
    }
}
