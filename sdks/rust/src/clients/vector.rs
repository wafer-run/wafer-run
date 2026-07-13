//! Typed client for the vector and embedding services.
//!
//! Vector index ops route to the `wafer-run/vector` block. The embedding
//! op (`embedding.embed`) routes to a caller-provided block name — any
//! block implementing the embedding service (e.g. `my-org/fastembed`).
//! All ops are buffered single-frame request/response. Index ops that
//! mutate state (`create_index`, `delete_index`, `upsert`, `delete`)
//! return an empty acknowledgement.

use wafer_block::{
    wire::vector::{
        CountRequest, CountResponse, CreateIndexRequest, DeleteIndexRequest, DeleteRequest,
        EmbedRequest, EmbedResponse, QueryRequest, QueryResponse, UpsertRequest,
    },
    ServiceOp, WaferError,
};

use super::common::{call, call_ack};

const VECTOR_BLOCK: &str = "wafer-run/vector";

/// Buffered: create a new vector index. The response is an empty
/// acknowledgement.
pub fn create_index(request: &CreateIndexRequest) -> Result<(), WaferError> {
    call_ack(VECTOR_BLOCK, ServiceOp::VECTOR_CREATE_INDEX, request)
}

/// Buffered: delete a vector index. The response is an empty acknowledgement.
pub fn delete_index(request: &DeleteIndexRequest) -> Result<(), WaferError> {
    call_ack(VECTOR_BLOCK, ServiceOp::VECTOR_DELETE_INDEX, request)
}

/// Buffered: upsert one or more entries into an index. The response is an
/// empty acknowledgement.
pub fn upsert(request: &UpsertRequest) -> Result<(), WaferError> {
    call_ack(VECTOR_BLOCK, ServiceOp::VECTOR_UPSERT, request)
}

/// Buffered: query an index. Returns the top-k matches per the request's
/// search mode (vector, keyword, or hybrid).
pub fn query(request: &QueryRequest) -> Result<QueryResponse, WaferError> {
    call(VECTOR_BLOCK, ServiceOp::VECTOR_QUERY, request)
}

/// Buffered: delete entries by id from an index. The response is an empty
/// acknowledgement.
pub fn delete(request: &DeleteRequest) -> Result<(), WaferError> {
    call_ack(VECTOR_BLOCK, ServiceOp::VECTOR_DELETE, request)
}

/// Buffered: count the number of entries in an index.
pub fn count(request: &CountRequest) -> Result<CountResponse, WaferError> {
    call(VECTOR_BLOCK, ServiceOp::VECTOR_COUNT, request)
}

/// Call an embedding block to embed the given texts.
///
/// `embedding_block` is the name of any block implementing the embedding
/// service (e.g. `my-org/fastembed`). The op itself is
/// [`ServiceOp::EMBEDDING_EMBED`]; the response carries the model name,
/// dimensionality, and one f32 vector per input text.
pub fn embed(embedding_block: &str, request: &EmbedRequest) -> Result<EmbedResponse, WaferError> {
    call(embedding_block, ServiceOp::EMBEDDING_EMBED, request)
}
