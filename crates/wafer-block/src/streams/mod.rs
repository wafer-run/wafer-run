//! Streaming primitives used by block handlers: an [`input::InputStream`] for
//! request body bytes and an [`output::OutputStream`] that emits chunked
//! response events.

/// Async byte-stream wrapping an incoming request body.
pub mod input;
/// Async event-stream representing a block's response.
pub mod output;
