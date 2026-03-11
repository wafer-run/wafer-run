mod block;
pub mod service;

pub use block::CryptoBlock;

#[cfg(feature = "crypto")]
pub fn register(w: &mut wafer_run::Wafer) {
    use std::sync::Arc;
    w.register_block("@wafer/crypto", Arc::new(CryptoBlock::new()));
}
