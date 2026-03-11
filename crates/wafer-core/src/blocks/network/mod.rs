mod block;
pub mod service;

pub use block::NetworkBlock;

pub fn register(w: &mut wafer_run::Wafer) {
    use std::sync::Arc;
    w.register_block(
        "@wafer/network",
        Arc::new(NetworkBlock::new(Arc::new(service::HttpNetworkService::new()))),
    );
}
