//! Platform-aware task spawning.
//!
//! `spawn_producer` runs a future concurrently:
//! - Native: `tokio::spawn` (requires tokio `rt` feature)
//! - wasm32: `wasm_bindgen_futures::spawn_local`

use std::future::Future;

/// Spawn a fire-and-forget producer task.
///
/// On native targets this delegates to `tokio::spawn`.
/// On wasm32 this delegates to `wasm_bindgen_futures::spawn_local`.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_producer<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(future);
}

#[cfg(target_arch = "wasm32")]
pub fn spawn_producer<F>(future: F)
where
    F: Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(future);
}
