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

use serde::{Deserialize, Serialize};
#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
use wafer_block::{common::ServiceOp, WaferError};

use super::{call_service, decode, dual_api, svc};
// Re-export types for callers.
pub use crate::interfaces::vector::service::{
    DistanceMetric, MetadataFilter, SearchMode, VectorEntry, VectorIndexConfig, VectorMatch,
};

const VECTOR_BLOCK: &str = "wafer-run/vector";

// --- Wire-format request types ---

#[derive(Serialize)]
struct CreateIndexReq<'a> {
    config: &'a VectorIndexConfig,
}

#[derive(Serialize)]
struct DeleteIndexReq<'a> {
    name: &'a str,
}

#[derive(Serialize)]
struct UpsertReq<'a> {
    index: &'a str,
    entries: &'a [VectorEntry],
}

#[derive(Serialize)]
struct QueryReq<'a> {
    index: &'a str,
    vector: &'a [f32],
    top_k: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    filter: Option<&'a MetadataFilter>,
    mode: SearchMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    keyword_query: Option<&'a str>,
}

#[derive(Serialize)]
struct DeleteReq<'a> {
    index: &'a str,
    ids: &'a [String],
}

#[derive(Serialize)]
struct CountReq<'a> {
    index: &'a str,
}

#[derive(Serialize)]
struct EmbedReq<'a> {
    texts: &'a [String],
}

// --- Wire-format response types ---

#[derive(Deserialize)]
struct QueryResp {
    matches: Vec<VectorMatch>,
}

#[derive(Deserialize)]
struct CountResp {
    count: u64,
}

#[derive(Deserialize)]
struct EmbedResp {
    model: String,
    dimensions: u32,
    vectors: Vec<Vec<f32>>,
}

// ===========================================================================
// Public API — generated as async (native) or sync (wasm-component)
// ===========================================================================

dual_api! {
    pub fn create_index(ctx, config: VectorIndexConfig) -> Result<(), WaferError> {
        svc!(
            ctx, VECTOR_BLOCK,
            ServiceOp::VECTOR_CREATE_INDEX,
            &CreateIndexReq { config: &config },
            None::<&str>,
            false,
            None::<&str>
        )?;
        Ok(())
    }

    pub fn delete_index(ctx, name: &str) -> Result<(), WaferError> {
        svc!(
            ctx, VECTOR_BLOCK,
            ServiceOp::VECTOR_DELETE_INDEX,
            &DeleteIndexReq { name },
            None::<&str>,
            false,
            None::<&str>
        )?;
        Ok(())
    }

    pub fn upsert(ctx, index: &str, entries: Vec<VectorEntry>) -> Result<(), WaferError> {
        svc!(
            ctx, VECTOR_BLOCK,
            ServiceOp::VECTOR_UPSERT,
            &UpsertReq { index, entries: &entries },
            None::<&str>,
            false,
            None::<&str>
        )?;
        Ok(())
    }

    pub fn query(
        ctx,
        index: &str,
        vector: Vec<f32>,
        top_k: usize,
        filter: Option<MetadataFilter>,
        mode: SearchMode,
        keyword_query: Option<String>,
    ) -> Result<Vec<VectorMatch>, WaferError> {
        let data = svc!(
            ctx, VECTOR_BLOCK,
            ServiceOp::VECTOR_QUERY,
            &QueryReq {
                index,
                vector: &vector,
                top_k,
                filter: filter.as_ref(),
                mode,
                keyword_query: keyword_query.as_deref(),
            },
            None::<&str>,
            false,
            None::<&str>
        )?;
        let resp: QueryResp = decode(&data)?;
        Ok(resp.matches)
    }

    pub fn delete(ctx, index: &str, ids: Vec<String>) -> Result<(), WaferError> {
        svc!(
            ctx, VECTOR_BLOCK,
            ServiceOp::VECTOR_DELETE,
            &DeleteReq { index, ids: &ids },
            None::<&str>,
            false,
            None::<&str>
        )?;
        Ok(())
    }

    pub fn count(ctx, index: &str) -> Result<u64, WaferError> {
        let data = svc!(
            ctx, VECTOR_BLOCK,
            ServiceOp::VECTOR_COUNT,
            &CountReq { index },
            None::<&str>,
            false,
            None::<&str>
        )?;
        let resp: CountResp = decode(&data)?;
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
        let data = svc!(
            ctx, embedding_block,
            ServiceOp::EMBEDDING_EMBED,
            &EmbedReq { texts: &texts },
            None::<&str>,
            false,
            None::<&str>
        )?;
        let resp: EmbedResp = decode(&data)?;
        Ok((resp.model, resp.dimensions, resp.vectors))
    }
}
