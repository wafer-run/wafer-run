#[cfg(not(target_arch = "wasm32"))]
mod block;
pub mod service;

#[cfg(not(target_arch = "wasm32"))]
pub use block::LoggerBlock;

#[cfg(not(target_arch = "wasm32"))]
pub fn register(w: &mut wafer_run::Wafer) {
    use std::sync::Arc;
    w.register_block("@wafer/logger", Arc::new(LoggerBlock::new()));
}
