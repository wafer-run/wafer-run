//! Native ONNX-based EmbeddingService for WAFER using fastembed-rs.
#![warn(missing_docs)]

mod batcher;
/// Concrete [`wafer_core::interfaces::vector::service::EmbeddingService`]
/// implementation that runs sentence-embedding models locally through the
/// fastembed-rs ONNX runtime bindings.
pub mod service;

pub use service::FastembedService;
