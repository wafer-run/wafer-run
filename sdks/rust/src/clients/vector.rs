//! Typed client for the vector and embedding services.
//!
//! Vector index ops route to the `wafer-run/vector` block. The embedding
//! op (`embedding.embed`) routes to a caller-provided block name — any
//! block implementing the embedding service (e.g. `suppers-ai/fastembed`).
//! All ops are buffered single-frame request/response. Index ops that
//! mutate state (`create_index`, `delete_index`, `upsert`, `delete`)
//! return an empty acknowledgement.

use wafer_block::{
    codec,
    wire::vector::{
        CountRequest, CountResponse, CreateIndexRequest, DeleteIndexRequest, DeleteRequest,
        EmbedRequest, EmbedResponse, QueryRequest, QueryResponse, UpsertRequest,
    },
    ServiceOp, WaferError,
};

use super::common::{collect_single_frame, consume_ack, open_buffered};

const VECTOR_BLOCK: &str = "wafer-run/vector";

/// Buffered: create a new vector index. The response is an empty
/// acknowledgement.
pub fn create_index(request: &CreateIndexRequest) -> Result<(), WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream =
        open_buffered(VECTOR_BLOCK, ServiceOp::VECTOR_CREATE_INDEX, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Buffered: delete a vector index. The response is an empty acknowledgement.
pub fn delete_index(request: &DeleteIndexRequest) -> Result<(), WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream =
        open_buffered(VECTOR_BLOCK, ServiceOp::VECTOR_DELETE_INDEX, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Buffered: upsert one or more entries into an index. The response is an
/// empty acknowledgement.
pub fn upsert(request: &UpsertRequest) -> Result<(), WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(VECTOR_BLOCK, ServiceOp::VECTOR_UPSERT, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Buffered: query an index. Returns the top-k matches per the request's
/// search mode (vector, keyword, or hybrid).
pub fn query(request: &QueryRequest) -> Result<QueryResponse, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(VECTOR_BLOCK, ServiceOp::VECTOR_QUERY, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "vector QUERY")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding vector QUERY response: {}", e.message),
        )
    })
}

/// Buffered: delete entries by id from an index. The response is an empty
/// acknowledgement.
pub fn delete(request: &DeleteRequest) -> Result<(), WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(VECTOR_BLOCK, ServiceOp::VECTOR_DELETE, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Buffered: count the number of entries in an index.
pub fn count(request: &CountRequest) -> Result<CountResponse, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(VECTOR_BLOCK, ServiceOp::VECTOR_COUNT, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "vector COUNT")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding vector COUNT response: {}", e.message),
        )
    })
}

/// Call an embedding block to embed the given texts.
///
/// `embedding_block` is the name of any block implementing the embedding
/// service (e.g. `suppers-ai/fastembed`). The op itself is
/// [`ServiceOp::EMBEDDING_EMBED`]; the response carries the model name,
/// dimensionality, and one f32 vector per input text.
pub fn embed(embedding_block: &str, request: &EmbedRequest) -> Result<EmbedResponse, WaferError> {
    let req_bytes = codec::encode(request)?;
    let mut response_stream =
        open_buffered(embedding_block, ServiceOp::EMBEDDING_EMBED, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, "embedding EMBED")?;
    codec::decode(&body).map_err(|e| {
        WaferError::new(
            e.code,
            format!("decoding embedding EMBED response: {}", e.message),
        )
    })
}
