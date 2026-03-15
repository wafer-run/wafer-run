mod block;
pub mod service;

pub use block::CryptoBlock;

pub fn register(w: &mut wafer_run::Wafer) {
    use std::sync::Arc;
    w.register_block("wafer-run/crypto", Arc::new(CryptoBlock::new()));
}
