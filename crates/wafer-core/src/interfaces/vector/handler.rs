//! Shared message handler logic for vector and embedding blocks.
//!
//! Any block implementing the `vector@v1` or `embedding@v1` interface can
//! delegate to these functions to avoid duplicating the message protocol
//! handling.

use wafer_block::{
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    wire::vector as wire,
    *,
};

use super::service::{self, EmbeddingService, VectorError, VectorService};
use crate::interfaces::handler_util::{decode_or_err, to_output};

// --- Wire <-> service conversions ---
//
// Field-identical types — conversion is mechanical. We keep them in the
// handler rather than implementing `From` on the service crate because the
// service trait is intentionally agnostic of the wire representation.

fn wire_metric_to_service(m: wire::DistanceMetric) -> service::DistanceMetric {
    match m {
        wire::DistanceMetric::Cosine => service::DistanceMetric::Cosine,
        wire::DistanceMetric::Euclidean => service::DistanceMetric::Euclidean,
        wire::DistanceMetric::DotProduct => service::DistanceMetric::DotProduct,
    }
}

fn wire_search_mode_to_service(m: wire::SearchMode) -> service::SearchMode {
    match m {
        wire::SearchMode::Vector => service::SearchMode::Vector,
        wire::SearchMode::Keyword => service::SearchMode::Keyword,
        wire::SearchMode::Hybrid => service::SearchMode::Hybrid,
    }
}

fn wire_index_config_to_service(c: wire::VectorIndexConfig) -> service::VectorIndexConfig {
    service::VectorIndexConfig {
        name: c.name,
        model: c.model,
        dimensions: c.dimensions,
        metric: wire_metric_to_service(c.metric),
        keyword_search: c.keyword_search,
    }
}

fn wire_entry_to_service(e: wire::VectorEntry) -> service::VectorEntry {
    service::VectorEntry {
        id: e.id,
        vector: e.vector,
        metadata: e.metadata,
        text: e.text,
    }
}

fn wire_metadata_filter_to_service(f: wire::MetadataFilter) -> service::MetadataFilter {
    service::MetadataFilter { equals: f.equals }
}

fn service_match_to_wire(m: service::VectorMatch) -> wire::VectorMatch {
    wire::VectorMatch {
        id: m.id,
        score: m.score,
        metadata: m.metadata,
    }
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
        | VectorError::KeywordQueryRequired(_)
        | VectorError::InvalidIndexName(_) => {
            WaferError::new(ErrorCode::INVALID_ARGUMENT, e.to_string())
        }
        VectorError::Internal(msg) => {
            tracing::error!(error = %msg, "vector internal error");
            WaferError::new(ErrorCode::INTERNAL, "internal vector error")
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
            match service
                .create_index(wire_index_config_to_service(req.config))
                .await
            {
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
            let entries: Vec<service::VectorEntry> =
                req.entries.into_iter().map(wire_entry_to_service).collect();
            match service.upsert(&req.index, entries).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(vector_error_to_wafer(e)),
            }
        }
        ServiceOp::VECTOR_QUERY => {
            let req = decode_or_err!(body, wire::QueryRequest, "vector.query");
            let filter = req.filter.map(wire_metadata_filter_to_service);
            let mode = wire_search_mode_to_service(req.mode);
            match service
                .query(
                    &req.index,
                    req.vector,
                    req.top_k,
                    filter,
                    mode,
                    req.keyword_query,
                )
                .await
            {
                Ok(matches) => {
                    let wire_matches: Vec<wire::VectorMatch> =
                        matches.into_iter().map(service_match_to_wire).collect();
                    to_output(&wire::QueryResponse {
                        matches: wire_matches,
                    })
                }
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
            ErrorCode::UNIMPLEMENTED,
            format!("unknown embedding operation: {other}"),
        )),
    }
}
