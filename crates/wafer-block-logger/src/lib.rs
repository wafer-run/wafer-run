mod block;
pub mod service;

pub use block::LoggerBlock;

pub fn register(w: &mut wafer_run::Wafer) {
    use std::sync::Arc;
    w.register_block("wafer-run/logger", Arc::new(LoggerBlock::new()));
}
