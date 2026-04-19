//! Platform-aware task spawning.
//!
//! `spawn_producer` runs a future concurrently:
//! - Native: `tokio::spawn` (requires tokio `rt` feature)
//! - wasm32 browser (wasm32-unknown-unknown): `wasm_bindgen_futures::spawn_local`
//! - wasm32-wasip1: not supported — WASI blocks are synchronous; panics if called

use std::future::Future;

/// Spawn a fire-and-forget producer task.
///
/// On native targets this delegates to `tokio::spawn`.
/// On wasm32 browser targets this delegates to `wasm_bindgen_futures::spawn_local`.
/// On wasm32-wasip1 (WASI) targets this panics — WASI blocks must not use
/// streaming `OutputStream::from_producer`.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_producer<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(future);
}

/// Browser WASM (wasm32-unknown-unknown): delegate to wasm_bindgen_futures.
#[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
pub fn spawn_producer<F>(future: F)
where
    F: Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(future);
}

/// WASI blocks (wasm32-wasip1) are synchronous — `from_producer` must not be called.
#[cfg(all(target_arch = "wasm32", target_os = "wasi"))]
pub fn spawn_producer<F>(_future: F)
where
    F: Future<Output = ()> + 'static,
{
    panic!("spawn_producer is not supported on wasm32-wasip1 (WASI); blocks must return GuestResult directly");
}
