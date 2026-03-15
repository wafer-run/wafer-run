mod block;
pub mod service;
#[cfg(feature = "toml")]
pub mod toml;

pub use block::ConfigBlock;

pub fn register(w: &mut wafer_run::Wafer) {
    use std::sync::Arc;
    w.register_block(
        "wafer-run/config",
        Arc::new(ConfigBlock::new(Some(Arc::new(service::EnvConfigService::new())))),
    );
}
