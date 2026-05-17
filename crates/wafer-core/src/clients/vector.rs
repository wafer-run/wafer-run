//! Typed client for `vector@v1` and `embedding@v1` service ops.
//!
//! Vector ops are routed to the `wafer-run/vector` block. Embedding calls
//! accept a caller-provided block name so app blocks can dispatch by model
//! (e.g. `suppers-ai/fastembed`, `suppers-ai/openai-embed`).
//!
//! WRAP access control is not applied at this layer. Callers (app blocks
//! such as `suppers-ai/vector`) enforce authentication and authorization
//! at the HTTP boundary. If per-index WRAP typing is needed later, add
//! `Vector` and `Embedding` variants to `ResourceType` in `wafer-block`.

#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
// Re-export wire types for callers — byte-identical to the legacy
// `interfaces::vector::service::*` types (the wire crate is the canonical
// home for these vector data types now).
pub use wafer_block::wire::vector::{
    DistanceMetric, MetadataFilter, SearchMode, VectorEntry, VectorIndexConfig, VectorMatch,
};
use wafer_block::{
    common::ServiceOp,
    wire::vector::{
        CountRequest, CountResponse, CountTokensRequest, CountTokensResponse, CreateIndexRequest,
        DeleteIndexRequest, DeleteRequest, EmbedRequest, EmbedResponse, QueryRequest,
        QueryResponse, UpsertRequest,
    },
    WaferError,
};

use super::{call_service, decode, dual_api, svc};

const VECTOR_BLOCK: &str = "wafer-run/vector";

// ===========================================================================
// Public API — generated as async (native) or sync (wasm-component)
// ===========================================================================

dual_api! {
    /// Create a vector index described by `config` on the vector block.
    pub fn create_index(ctx, config: VectorIndexConfig) -> Result<(), WaferError> {
        let req = CreateIndexRequest { config };
        svc!(
            ctx, VECTOR_BLOCK,
            ServiceOp::VECTOR_CREATE_INDEX,
            &req,
            None::<&str>,
            false,
            None::<&str>
        )?;
        Ok(())
    }

    /// Drop the vector index `name`.
    pub fn delete_index(ctx, name: &str) -> Result<(), WaferError> {
        let req = DeleteIndexRequest { name: name.to_string() };
        svc!(
            ctx, VECTOR_BLOCK,
            ServiceOp::VECTOR_DELETE_INDEX,
            &req,
            None::<&str>,
            false,
            None::<&str>
        )?;
        Ok(())
    }

    /// Insert or replace `entries` in the named vector `index`.
    pub fn upsert(ctx, index: &str, entries: Vec<VectorEntry>) -> Result<(), WaferError> {
        let req = UpsertRequest { index: index.to_string(), entries };
        svc!(
            ctx, VECTOR_BLOCK,
            ServiceOp::VECTOR_UPSERT,
            &req,
            None::<&str>,
            false,
            None::<&str>
        )?;
        Ok(())
    }

    /// Search `index` for the `top_k` nearest matches under the chosen `mode`
    /// (vector / keyword / hybrid), optionally constrained by `filter`.
    pub fn query(
        ctx,
        index: &str,
        vector: Vec<f32>,
        top_k: usize,
        filter: Option<MetadataFilter>,
        mode: SearchMode,
        keyword_query: Option<String>,
    ) -> Result<Vec<VectorMatch>, WaferError> {
        let req = QueryRequest {
            index: index.to_string(),
            vector,
            top_k,
            filter,
            mode,
            keyword_query,
        };
        let data = svc!(
            ctx, VECTOR_BLOCK,
            ServiceOp::VECTOR_QUERY,
            &req,
            None::<&str>,
            false,
            None::<&str>
        )?;
        let resp: QueryResponse = decode(&data)?;
        Ok(resp.matches)
    }

    /// Remove the entries whose ids are listed in `ids` from `index`.
    pub fn delete(ctx, index: &str, ids: Vec<String>) -> Result<(), WaferError> {
        let req = DeleteRequest { index: index.to_string(), ids };
        svc!(
            ctx, VECTOR_BLOCK,
            ServiceOp::VECTOR_DELETE,
            &req,
            None::<&str>,
            false,
            None::<&str>
        )?;
        Ok(())
    }

    /// Return the number of entries currently stored in `index`.
    pub fn count(ctx, index: &str) -> Result<u64, WaferError> {
        let req = CountRequest { index: index.to_string() };
        let data = svc!(
            ctx, VECTOR_BLOCK,
            ServiceOp::VECTOR_COUNT,
            &req,
            None::<&str>,
            false,
            None::<&str>
        )?;
        let resp: CountResponse = decode(&data)?;
        Ok(resp.count)
    }

    /// Call an embedding block to embed the given texts.
    ///
    /// `embedding_block` is the name of any block implementing `embedding@v1`
    /// (e.g. `suppers-ai/fastembed`). Returns `(model, dimensions, vectors)`.
    pub fn embed(
        ctx,
        embedding_block: &str,
        texts: Vec<String>,
    ) -> Result<(String, u32, Vec<Vec<f32>>), WaferError> {
        let req = EmbedRequest { texts };
        let data = svc!(
            ctx, embedding_block,
            ServiceOp::EMBEDDING_EMBED,
            &req,
            None::<&str>,
            false,
            None::<&str>
        )?;
        let resp: EmbedResponse = decode(&data)?;
        Ok((resp.model, resp.dimensions, resp.vectors))
    }

    /// Count the tokens an embedding block's tokenizer would produce for `text`.
    ///
    /// Used by ingest pipelines to size chunks by real BPE tokens rather than
    /// whitespace approximation. Cheap to call across the block boundary —
    /// the embedding block runs only the tokenizer, no model inference.
    pub fn count_tokens(
        ctx,
        embedding_block: &str,
        text: String,
    ) -> Result<u64, WaferError> {
        let req = CountTokensRequest { text };
        let data = svc!(
            ctx, embedding_block,
            ServiceOp::EMBEDDING_COUNT_TOKENS,
            &req,
            None::<&str>,
            false,
            None::<&str>
        )?;
        let resp: CountTokensResponse = decode(&data)?;
        Ok(resp.tokens)
    }
}
